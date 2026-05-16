//! Parse-cache integration (ADR-0074 Phase 2b).
//!
//! Wraps the per-file lex+parse loop with a content-addressed cache. On
//! cache hit, the AST is deserialized and its file-local Spurs are
//! re-interned into the build's shared `ThreadedRodeo`. On miss, parsing
//! runs normally and the resulting AST + per-file interner snapshot is
//! written back to the cache.
//!
//! ## Design
//!
//! Each file is parsed into its own `ThreadedRodeo` (rather than the
//! historical shared interner). After parse, the per-file interner is
//! snapshotted, the AST is cached, and the snapshot is then replayed
//! into the build's shared interner — producing a remap table that the
//! `RemapSpurs` walker uses to substitute the AST's Spurs into the
//! build's numbering.
//!
//! This per-file architecture is what makes the cache key independent
//! of compilation order: the snapshot for `foo.gruel` only contains
//! strings the parser of `foo.gruel` interned, regardless of which
//! other files were parsed in the same build.

use lasso::ThreadedRodeo;
use tracing::{debug, info, info_span, warn};

use gruel_cache::{
    CacheKey, CacheKind, CacheStore, CachedParseOutput, Hasher, InternerSnapshot, RemapSpurs,
    blake3_bytes,
};
use gruel_lexer::Lexer;
use gruel_parser::Parser;
use gruel_util::{CompileErrors, MultiErrorResult, PreviewFeatures};

use crate::{ParsedFile, ParsedProgram, SourceFile};
#[cfg(test)]
use gruel_util::FileId;

/// Hit/miss counts for one parse-pipeline invocation. Surfaced to
/// `--time-passes` so users can see whether the cache is doing work.
#[derive(Debug, Clone, Default)]
pub struct ParseCacheStats {
    pub hits: usize,
    pub misses: usize,
}

impl ParseCacheStats {
    pub fn total(&self) -> usize {
        self.hits + self.misses
    }
}

/// Compute the parse-cache key for one source file.
///
/// `build_fp` mixes in the compiler binary hash, target, opt level, and
/// preview-feature set; `file_fp` is the BLAKE3 of the source bytes. The
/// resulting key is stable as long as both stay constant.
pub fn parse_key(build_fp: &CacheKey, source_bytes: &[u8]) -> CacheKey {
    let file_fp = blake3_bytes(source_bytes);
    let mut h = Hasher::new();
    h.update(build_fp.as_bytes());
    h.update(file_fp.as_bytes());
    h.finalize()
}

/// Run the parse pipeline with cache lookup/store enabled.
///
/// Behavior:
/// - For each `SourceFile`, compute `parse_key` and probe the cache.
/// - On hit: deserialize `CachedParseOutput`, re-intern its snapshot
///   into the build's shared `ThreadedRodeo`, and walk the AST to
///   substitute Spurs via the remap. Skip lex+parse for that file.
/// - On miss: lex+parse into a fresh per-file interner, snapshot it,
///   store the cached output, then merge into the build interner the
///   same way as a hit (ensuring the merge path is exercised on every
///   build, not just hits).
///
/// Returns the parsed program plus per-stage cache stats.
pub fn parse_all_files_cached(
    sources: &[SourceFile<'_>],
    preview_features: &PreviewFeatures,
    cache: &CacheStore,
    build_fp: &CacheKey,
) -> MultiErrorResult<(ParsedProgram, ParseCacheStats)> {
    let build_interner = ThreadedRodeo::new();
    let (files, stats) =
        parse_files_into(&build_interner, sources, preview_features, cache, build_fp)?;
    Ok((
        ParsedProgram {
            files,
            interner: build_interner,
        },
        stats,
    ))
}

/// Like [`parse_all_files_cached`], but appends parsed files into a
/// caller-supplied build interner. Used by `CompilationUnit::parse` to
/// share one `ThreadedRodeo` between the synthetic prelude (parsed
/// uncached, the existing path) and user files (parsed cached, this
/// path).
pub fn parse_files_into(
    build_interner: &ThreadedRodeo,
    sources: &[SourceFile<'_>],
    preview_features: &PreviewFeatures,
    cache: &CacheStore,
    build_fp: &CacheKey,
) -> MultiErrorResult<(Vec<ParsedFile>, ParseCacheStats)> {
    let _span = info_span!("parse_cached", file_count = sources.len()).entered();

    let mut stats = ParseCacheStats::default();
    let mut parsed_files = Vec::with_capacity(sources.len());

    for source in sources {
        let key = parse_key(build_fp, source.source.as_bytes());

        // Try the cache first.
        let (mut ast, file_interner_snap) = match cache.get(CacheKind::Parse, &key) {
            Ok(Some(bytes)) => match CachedParseOutput::decode(&bytes) {
                Ok(cached) => {
                    debug!(path = %source.path, "parse-cache hit");
                    stats.hits += 1;
                    (cached.ast, cached.interner)
                }
                Err(e) => {
                    // Correctness fallback: cache miss on any deserialize error.
                    warn!(
                        path = %source.path,
                        error = %e,
                        "parse-cache deserialize failed; recomputing"
                    );
                    stats.misses += 1;
                    parse_uncached(source, preview_features)?
                }
            },
            Ok(None) => {
                debug!(path = %source.path, "parse-cache miss");
                stats.misses += 1;
                let (ast, snap) = parse_uncached(source, preview_features)?;
                // Best-effort store; cache write failure is not a build error.
                let cached = CachedParseOutput {
                    interner: snap.clone(),
                    ast: ast.clone(),
                };
                match cached.encode() {
                    Ok(bytes) => {
                        if let Err(e) = cache.put(CacheKind::Parse, &key, &bytes) {
                            warn!(error = %e, "parse-cache write failed");
                        }
                    }
                    Err(e) => warn!(error = %e, "parse-cache encode failed"),
                }
                (ast, snap)
            }
            Err(e) => {
                warn!(error = %e, "parse-cache read failed; recomputing");
                stats.misses += 1;
                parse_uncached(source, preview_features)?
            }
        };

        // Merge per-file interner snapshot into the build interner; remap
        // the AST's Spurs from cached numbering to build numbering. The
        // path is identical for hits and misses, so any latent bug in
        // remap shows up on cold builds too.
        let remap = file_interner_snap.restore_into(build_interner);
        ast.remap_spurs(&remap);

        parsed_files.push(ParsedFile {
            path: source.path.to_string(),
            file_id: source.file_id,
            ast,
            // Per-file interner field is API-compat only; the real
            // interner is the build-shared one in ParsedProgram.
            interner: ThreadedRodeo::new(),
        });
    }

    info!(
        hits = stats.hits,
        misses = stats.misses,
        files = sources.len(),
        "parse cache pass complete"
    );

    Ok((parsed_files, stats))
}

/// Lex + parse one file into its own fresh `ThreadedRodeo`, returning
/// the AST and a snapshot of the per-file interner.
///
/// Mirrors what `parse_all_files_with_preview` does for one file, but
/// uses a per-file interner so the cache snapshot is independent of
/// other files in the same build.
fn parse_uncached(
    source: &SourceFile<'_>,
    preview_features: &PreviewFeatures,
) -> MultiErrorResult<(gruel_parser::Ast, InternerSnapshot)> {
    let file_interner = ThreadedRodeo::new();

    let lexer = Lexer::with_interner_and_file_id(source.source, file_interner, source.file_id);
    let (tokens, file_interner) = lexer.tokenize().map_err(CompileErrors::from)?;

    let parser = Parser::new(tokens, file_interner).with_preview_features(preview_features.clone());
    let (ast, file_interner) = parser.parse()?;

    let snapshot = InternerSnapshot::capture(&file_interner);
    Ok((ast, snapshot))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn fake_build_fp() -> CacheKey {
        blake3_bytes(b"fake-compiler-fingerprint")
    }

    #[test]
    fn cold_then_warm_run_produce_identical_asts() {
        let tmp = TempDir::new().unwrap();
        let cache = CacheStore::open(tmp.path().join("cache")).unwrap();
        let build_fp = fake_build_fp();

        let src = "fn main() -> i32 { 42 }";
        let sources = vec![SourceFile::new("main.gruel", src, FileId::new(1))];

        // Cold: cache miss expected.
        let (cold_program, cold_stats) =
            parse_all_files_cached(&sources, &PreviewFeatures::default(), &cache, &build_fp)
                .expect("cold parse should succeed");
        assert_eq!(cold_stats.hits, 0);
        assert_eq!(cold_stats.misses, 1);
        assert_eq!(cold_program.files.len(), 1);

        // Warm: cache hit expected; AST should structurally match.
        let (warm_program, warm_stats) =
            parse_all_files_cached(&sources, &PreviewFeatures::default(), &cache, &build_fp)
                .expect("warm parse should succeed");
        assert_eq!(warm_stats.hits, 1);
        assert_eq!(warm_stats.misses, 0);
        assert_eq!(
            cold_program.files[0].ast.items.len(),
            warm_program.files[0].ast.items.len(),
        );
    }

    /// ADR-0088 regression test: ensures every Spur in a re-loaded AST
    /// is correctly remapped into the build interner.
    ///
    /// The original bug was that `RemapSpurs for MethodSig` didn't walk
    /// the `directives` field, so cached directive-arg Spurs leaked
    /// through with their cached numbering. The leak only manifested
    /// when the build interner already contained *other* strings (e.g.
    /// the prelude was loaded first) — the un-remapped Spur then
    /// resolved to whichever build-interner string happened to occupy
    /// that slot. To reproduce: pre-warm a build interner with strings
    /// the cached file doesn't contain, then parse-load the cached AST
    /// into it, and verify that resolving the cached directive's args
    /// yields the *source* spelling, not whatever happened to be at the
    /// collision index.
    #[test]
    fn cached_ast_remaps_directives_into_warmed_build_interner() {
        let tmp = TempDir::new().unwrap();
        let cache = CacheStore::open(tmp.path().join("cache")).unwrap();
        let build_fp = fake_build_fp();

        // Build a file whose only directive arg is the unique token
        // "totally_unique_marker_name". If RemapSpurs misses any field,
        // the warm reload's resolved arg will land on whatever the
        // un-remapped Spur happens to point at in the pre-warmed
        // build interner — almost certainly NOT this string.
        let src = r#"
interface Bad {
    @mark(totally_unique_marker_name) fn foo(self) -> i32;
}
fn main() -> i32 { 0 }
"#;
        let sources = vec![SourceFile::new("bad.gruel", src, FileId::new(1))];

        // Cold parse — populates the cache.
        let _ = parse_all_files_cached(&sources, &PreviewFeatures::default(), &cache, &build_fp)
            .expect("cold parse should succeed");

        // Warm path: pre-warm a fresh build interner with unrelated
        // strings (simulating the prelude having been parsed first),
        // then load the cached AST into it via `parse_files_into`.
        let build_interner = ThreadedRodeo::new();
        for s in [
            "prelude_padding_0",
            "prelude_padding_1",
            "prelude_padding_2",
            "prelude_padding_3",
            "prelude_padding_4",
            "prelude_padding_5",
            "prelude_padding_6",
            "prelude_padding_7",
            "prelude_padding_8",
            "prelude_padding_9",
        ] {
            build_interner.get_or_intern(s);
        }

        let (warm_files, stats) = parse_files_into(
            &build_interner,
            &sources,
            &PreviewFeatures::default(),
            &cache,
            &build_fp,
        )
        .expect("warm parse should succeed");
        assert_eq!(stats.hits, 1);

        // Drill down: Interface → first MethodSig → first directive →
        // first arg. After RemapSpurs walks the cached MethodSig's
        // directives list, this arg must resolve to the source spelling
        // in the warmed build interner.
        let item = &warm_files[0].ast.items[0];
        let iface = match item {
            gruel_parser::ast::Item::Interface(i) => i,
            other => panic!("expected Interface, got {:?}", other),
        };
        let sig = &iface.methods[0];
        assert_eq!(sig.directives.len(), 1, "expected one @mark directive");
        let d = &sig.directives[0];
        assert_eq!(build_interner.resolve(&d.name.name), "mark");
        assert_eq!(d.args.len(), 1, "expected one directive arg");
        match &d.args[0] {
            gruel_parser::ast::DirectiveArg::Ident(i) => {
                assert_eq!(
                    build_interner.resolve(&i.name),
                    "totally_unique_marker_name",
                    "cached directive arg leaked unremapped Spur into the warmed build interner"
                );
            }
            other => panic!("expected Ident arg, got {:?}", other),
        }
    }

    #[test]
    fn editing_source_invalidates_only_changed_file() {
        let tmp = TempDir::new().unwrap();
        let cache = CacheStore::open(tmp.path().join("cache")).unwrap();
        let build_fp = fake_build_fp();

        // Two files, both miss on first build.
        let a = SourceFile::new("a.gruel", "fn a() -> i32 { 1 }", FileId::new(1));
        let b = SourceFile::new("b.gruel", "fn b() -> i32 { 2 }", FileId::new(2));

        let (_, cold_stats) = parse_all_files_cached(
            &[a.clone(), b.clone()],
            &PreviewFeatures::default(),
            &cache,
            &build_fp,
        )
        .unwrap();
        assert_eq!(cold_stats.misses, 2);

        // Modify a, leave b unchanged.
        let a2 = SourceFile::new("a.gruel", "fn a() -> i32 { 99 }", FileId::new(1));
        let (_, warm_stats) =
            parse_all_files_cached(&[a2, b], &PreviewFeatures::default(), &cache, &build_fp)
                .unwrap();
        // a missed, b hit.
        assert_eq!(warm_stats.hits, 1);
        assert_eq!(warm_stats.misses, 1);
    }
}
