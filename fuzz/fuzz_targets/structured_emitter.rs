#![no_main]
use gruel_codegen::x86_64::Emitter;
use gruel_fuzz::ArbitraryX86Mir;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|mir: ArbitraryX86Mir| {
    let emitter = Emitter::new(&mir.0, 0, 0, 0, &[], &[]);
    let _ = emitter.emit();
});
