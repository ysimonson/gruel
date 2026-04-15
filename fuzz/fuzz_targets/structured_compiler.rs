#![no_main]
use gruel_fuzz::GruelProgram;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|prog: GruelProgram| {
    let _ = gruel_compiler::compile_frontend(&prog.0);
});
