+++
title = "Thread Spawn"
weight = 17
+++

# Thread Spawn

This section describes the `@spawn` intrinsic and the `JoinHandle(R)`
prelude type (ADR-0084).

## `@spawn(fn, arg)`

{{ rule(id="4.17:1", cat="normative") }}

```ebnf
spawn = "@spawn" "(" fn_ref "," argument ")" ;
fn_ref = IDENT ;
```

{{ rule(id="4.17:2", cat="normative") }}

`@spawn(fn, arg)` runs `fn` on a new thread with `arg` as its sole
parameter and returns a linear `JoinHandle(R)` where `R` is `fn`'s
return type. The handle **MUST** be consumed via `join(self) -> R`
(linearity, ADR-0067).

{{ rule(id="4.17:3", cat="legality-rule") }}

The first argument **MUST** be a top-level function name. Methods,
anonymous functions, and comptime-bound function values are not
permitted.

{{ rule(id="4.17:4", cat="legality-rule") }}

The named function **MUST** take exactly one parameter. Multi-input
workers wrap their inputs in a tuple or struct on the caller's side.

{{ rule(id="4.17:5", cat="legality-rule") }}

The argument's type **MUST** match the function's parameter type
under the regular bidirectional unification rules.

{{ rule(id="4.17:6", cat="legality-rule") }}

The function's parameter type **MUST** be classified at least `Send`
on the trichotomy (ADR-0084 §3.15) — a structurally `Unsend` parameter
makes the spawn unsafe and is rejected at compile time. The
`@mark(checked_send)` and `@mark(checked_sync)` overrides on the
parameter's type lift the structural classification.

{{ rule(id="4.17:7", cat="legality-rule") }}

The function's parameter type **MUST NOT** be `Linear`. Linear values
are per-thread today; future work will lift this restriction.

{{ rule(id="4.17:8", cat="legality-rule") }}

The function's parameter type **MUST NOT** be a `Ref(T)` or
`MutRef(T)`. References are scope-bound (ADR-0076), so the spawned
thread cannot outlive the caller's stack frame.

{{ rule(id="4.17:9", cat="legality-rule") }}

The function's return type **MUST** be classified at least `Send`.
The join site transfers the result back across the thread boundary,
which would be unsafe for an `Unsend` return.

{{ rule(id="4.17:10", cat="dynamic-semantics") }}

A panic in the spawned function aborts the whole process. Future
work may add a `Result`-typed join.

{{ rule(id="4.17:11", cat="example") }}

```gruel
fn worker(input: i32) -> i32 {
    input * 2
}

fn main() -> i32 {
    let h = @spawn(worker, 21);
    h.join()  // 42
}
```

## `JoinHandle(R)`

{{ rule(id="4.17:12", cat="normative") }}

`JoinHandle(R)` is a prelude-resident parameterized type returned by
`@spawn`. It carries the bookkeeping for one spawned thread.

{{ rule(id="4.17:13", cat="normative") }}

The `JoinHandle(R)` posture is `Linear`. The handle **MUST** be
consumed via `join(self) -> R`; dropping it without consumption is a
compile-time error.

{{ rule(id="4.17:14", cat="normative") }}

The `JoinHandle(R)` thread-safety classification is `Send`,
unconditionally. The struct stores an opaque thread-handle pointer
(not the `R` value), and the `R` value is checked to be at least
`Send` at the `@spawn` site, so `JoinHandle(R)` is always paired with
a `Send` `R` by construction. The handle itself **MAY** be moved to
another thread to join from there.

{{ rule(id="4.17:15", cat="normative") }}

`JoinHandle::join(self) -> R` consumes the handle and blocks until
the spawned function returns, yielding the result.

{{ rule(id="4.17:16", cat="example") }}

```gruel
fn worker(input: i32) -> i32 { input + 1 }

fn main() -> i32 {
    let h: JoinHandle(i32) = @spawn(worker, 41);
    let result: i32 = h.join();
    result  // 42
}
```
