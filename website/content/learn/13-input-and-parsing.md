+++
title = "Input and Parsing"
weight = 14
template = "learn/page.html"
+++

# Input and Parsing

Gruel reads input from the user with `read_line()` and converts strings to numbers with the `parse_*` prelude fns. These used to be `@`-prefixed intrinsics; ADR-0087 moved them to the prelude so they're now ordinary library fns.

## Reading a Line

`read_line()` reads one line from standard input and returns the bytes as a `Vec(u8)` (newline stripped). Wrap the result in a `String` if you want text:

```gruel
fn main() -> i32 {
    @dbg("What is your name?");
    let bytes = read_line();
    let name = checked { String::from_utf8_unchecked(bytes) };
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

If EOF is reached with no input, the loop terminates and `read_line()` returns an empty `Vec(u8)`. A partial final line (no trailing newline) is returned as-is.

## Parsing Integers

Use `parse_i32`, `parse_i64`, `parse_u32`, or `parse_u64` to convert a string to a number. The string is passed by reference (`&s`) so it remains usable after the call:

```gruel
fn main() -> i32 {
    @dbg("Enter a number:");
    let bytes = read_line();
    let input = checked { String::from_utf8_unchecked(bytes) };
    let n = parse_i32(&input);
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

## All Parsing Functions

| Function | Input | Returns |
|-----------|-------|---------|
| `parse_i32(&s)` | `Ref(String)` | `i32` |
| `parse_i64(&s)` | `Ref(String)` | `i64` |
| `parse_u32(&s)` | `Ref(String)` | `u32` |
| `parse_u64(&s)` | `Ref(String)` | `u64` |

The input must be exactly decimal digits with an optional leading `-` (signed only). No whitespace, no underscores, no prefixes like `0x`.

## A Complete Example

Here's a program that reads two numbers and prints their sum:

```gruel
fn main() -> i32 {
    @dbg("First number:");
    let s1 = checked { String::from_utf8_unchecked(read_line()) };
    let a = parse_i32(&s1);

    @dbg("Second number:");
    let s2 = checked { String::from_utf8_unchecked(read_line()) };
    let b = parse_i32(&s2);

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

For programs that need random values, use `random_u32()` or `random_u64()`:

```gruel
fn main() -> i32 {
    let r = random_u32();
    @dbg(r);  // prints a random 32-bit number
    0
}
```

These read from the platform's entropy source (`getrandom` on Linux, `getentropy` on macOS) and are cryptographically secure, suitable for both general use and security-sensitive contexts like key generation.

## Building an Interactive Program

Combining input, parsing, and control flow:

```gruel
fn main() -> i32 {
    @dbg("Guess the number (between 1 and 10):");
    let secret = 7;
    let s = checked { String::from_utf8_unchecked(read_line()) };
    let guess = parse_i32(&s);

    if guess == secret {
        @dbg("Correct!");
    } else {
        @dbg("Wrong!");
        @dbg(secret);
    }

    0
}
```
