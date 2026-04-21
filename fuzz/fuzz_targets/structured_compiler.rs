#![no_main]
use gruel_fuzz::{GruelProgram, uses_extended_numeric_types};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|prog: GruelProgram| {
    if uses_extended_numeric_types(&prog.0) {
        let mut features = gruel_compiler::PreviewFeatures::new();
        features.insert(gruel_compiler::PreviewFeature::ExtendedNumericTypes);
        let _ = gruel_compiler::compile_frontend_with_options(&prog.0, &features);
    } else {
        let _ = gruel_compiler::compile_frontend(&prog.0);
    }
});
