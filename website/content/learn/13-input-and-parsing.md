+++
title = "Input and Parsing"
weight = 14
template = "learn/page.html"
+++

# Input and Parsing

Gruel can read input from the user with `@read_line` and convert strings to numbers with the `@parse_*` intrinsics.

## Reading a Line

`@read_line()` reads one line from standard input and returns it as a `String`, with the newline stripped:

```gruel
fn main() -> i32 {
    @dbg("What is your name?");
    let name = @read_line();
    @dbg("Hello,");
    @dbg(name);
    0
}
```

Running it:
```
$ ./program
What is your name?
Alice
Hello,
Alice
```

If EOF is reached with no input, the program panics with "unexpected end of input". A partial final line (no trailing newline) is returned as-is.

## Parsing Integers

Use `@parse_i32`, `@parse_i64`, `@parse_u32`, or `@parse_u64` to convert a string to a number. These borrow the string so it remains usable after the call:

```gruel
fn main() -> i32 {
    @dbg("Enter a number:");
    let input = @read_line();
    let n = @parse_i32(input);
    @dbg(n * 2);
    0
}
```

Running it:
```
$ ./program
Enter a number:
21
42
```

If the string contains anything other than digits (and an optional leading `-` for signed types), the program panics with a clear error message.

## All Parsing Intrinsics

| Intrinsic | Input | Returns |
|-----------|-------|---------|
| `@parse_i32(s)` | `String` | `i32` |
| `@parse_i64(s)` | `String` | `i64` |
| `@parse_u32(s)` | `String` | `u32` |
| `@parse_u64(s)` | `String` | `u64` |

The input must be exactly decimal digits with an optional leading `-` (signed only). No whitespace, no underscores, no prefixes like `0x`.

## A Complete Example

Here's a program that reads two numbers and prints their sum:

```gruel
fn main() -> i32 {
    @dbg("First number:");
    let a = @parse_i32(@read_line());

    @dbg("Second number:");
    let b = @parse_i32(@read_line());

    let sum = a + b;
    @dbg(sum);

    0
}
```

Running it:
```
$ ./program
First number:
10
Second number:
32
42
```

## Random Numbers

For programs that need random values, use `@random_u32()` or `@random_u64()`:

```gruel
fn main() -> i32 {
    let r = @random_u32();
    @dbg(r);  // prints a random 32-bit number
    0
}
```

These read from the platform's entropy source (getrandom on Linux, getentropy on macOS), so they're suitable for random number generation but not for cryptographic use.

## Building an Interactive Program

Combining input, parsing, and control flow:

```gruel
fn main() -> i32 {
    @dbg("Guess the number (between 1 and 10):");
    let secret = 7;
    let guess = @parse_i32(@read_line());

    if guess == secret {
        @dbg("Correct!");
    } else {
        @dbg("Wrong!");
        @dbg(secret);
    }

    0
}
```
