#![no_main]
use gruel_fuzz::MaybeInvalidProgram;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|prog: MaybeInvalidProgram| {
    let _ = gruel_compiler::compile_frontend(&prog.0);
});
