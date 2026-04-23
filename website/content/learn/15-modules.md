+++
title = "Modules"
weight = 16
template = "learn/page.html"
+++

# Modules

As programs grow, you'll want to split code across multiple files. Gruel's module system is simple: every `.gruel` file is a module, and you import it with `@import`.

## Importing a File

Use `@import("path/to/file.gruel")` to import another file. The result is a value containing all the `pub` declarations from that file:

```gruel
// math.gruel
pub fn add(a: i32, b: i32) -> i32 {
    a + b
}

pub fn mul(a: i32, b: i32) -> i32 {
    a * b
}
```

```gruel
// main.gruel
const math = @import("math.gruel");

fn main() -> i32 {
    let sum = math.add(3, 4);
    let product = math.mul(3, 4);

    @dbg(sum);      // prints: 7
    @dbg(product);  // prints: 12

    sum
}
```

Compile both files together and the import is resolved automatically:

```bash
cargo run -p gruel -- main.gruel math.gruel -o program
./program
```

## Public and Private

Only declarations marked `pub` are accessible from other modules. Everything else is private:

```gruel
// utils.gruel
pub fn public_helper(x: i32) -> i32 {
    double(x) + 1
}

fn double(x: i32) -> i32 {  // private — not accessible from outside
    x * 2
}
```

```gruel
// main.gruel
const utils = @import("utils.gruel");

fn main() -> i32 {
    utils.public_helper(5)  // OK: 11
    // utils.double(5)      // ERROR: `double` is private
}
```

## Modules and Structs

Modules work just like structs at the type level. You can pass a module to a function, store it in a variable, or use its associated functions—it all uses the same dot syntax you already know.

```gruel
// shapes.gruel
pub struct Circle {
    radius: i32,
}

pub fn area(c: Circle) -> i32 {
    // approximate: pi * r^2 ≈ 3 * r^2
    3 * c.radius * c.radius
}
```

```gruel
// main.gruel
const shapes = @import("shapes.gruel");

fn main() -> i32 {
    let c = shapes.Circle { radius: 5 };
    let a = shapes.area(c);
    @dbg(a);  // prints: 75
    0
}
```

## Organizing Larger Projects

For larger projects, group related files in a directory. Create an `_index.gruel` (or a main file) that re-exports what the rest of the project needs:

```
project/
├── main.gruel
├── math/
│   ├── arithmetic.gruel
│   └── geometry.gruel
└── io/
    └── input.gruel
```

```gruel
// main.gruel
const arithmetic = @import("math/arithmetic.gruel");
const geometry   = @import("math/geometry.gruel");

fn main() -> i32 {
    arithmetic.add(1, 2)
}
```

## Compiling Multiple Files

Pass all source files to the compiler. Order doesn't matter—the compiler figures out dependencies:

```bash
# Explicit files
cargo run -p gruel -- main.gruel math/arithmetic.gruel math/geometry.gruel -o program

# Or use shell glob expansion
cargo run -p gruel -- main.gruel math/*.gruel -o program
```

The `-o` flag is required when compiling more than one file.
