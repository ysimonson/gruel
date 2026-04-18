+++
title = "Strings"
weight = 12
template = "tutorial/page.html"
+++

# Strings

Gruel's `String` type holds a sequence of bytes, conventionally UTF-8. Strings own their data and clean up automatically when they go out of scope.

## String Literals

String literals have type `String`:

```gruel
fn main() -> i32 {
    let greeting = "hello";
    @dbg(greeting);  // prints: hello

    if greeting == "hello" {
        @dbg(1);  // prints: 1
    }

    0
}
```

## Move Semantics

Like structs, strings move by default—passing a string to a function transfers ownership:

```gruel
fn print_it(s: String) {
    @dbg(s);
}

fn main() -> i32 {
    let s = "hello";
    print_it(s);     // s moves here
    // print_it(s);  // ERROR: use of moved value
    0
}
```

## Building Strings

Create an empty string with `String::new()`, then append with `push_str`:

```gruel
fn main() -> i32 {
    let mut s = String::new();
    s.push_str("hello");
    s.push_str(", ");
    s.push_str("world!");

    @dbg(s);  // prints: hello, world!

    0
}
```

## Appending Individual Bytes

Use `push` to append a single byte:

```gruel
fn main() -> i32 {
    let mut s = String::new();
    s.push_str("hello");
    s.push(33);  // 33 is '!'

    @dbg(s);  // prints: hello!

    0
}
```

## Querying a String

Query methods use `borrow self`, so they don't consume the string:

```gruel
fn main() -> i32 {
    let s = "hello, world!";

    @dbg(s.len());       // prints: 13
    @dbg(s.is_empty());  // prints: false (0)

    let empty = String::new();
    @dbg(empty.is_empty());  // prints: true (1)

    0
}
```

## Cloning

To make an independent copy of a string, use `clone`:

```gruel
fn main() -> i32 {
    let a = "hello";
    let b = a.clone();  // b is a separate copy

    // Both a and b are valid independent strings
    @dbg(a);  // prints: hello
    @dbg(b);  // prints: hello

    0
}
```

Clone is explicit because it allocates memory. Use it only when you need two independent strings.

## Automatic Cleanup

When a `String` goes out of scope, its memory is freed automatically. You never call `free` manually:

```gruel
fn build_string() -> String {
    let mut s = String::new();
    s.push_str("built inside a function");
    s  // returned, not dropped
}

fn main() -> i32 {
    let s = build_string();
    @dbg(s);  // prints: built inside a function
    // s is dropped here when main returns, memory freed
    0
}
```

## Custom Destructors

If your struct holds a String or other resource that needs cleanup, you can define a custom destructor with `drop fn`. See [Destructors](/tutorial/destructors/) for details on drop semantics, drop order, and how to write your own.

## Pre-allocating Capacity

If you know roughly how large a string will be, pre-allocate to avoid repeated reallocations:

```gruel
fn main() -> i32 {
    let mut s = String::with_capacity(64);
    s.push_str("first part");
    s.push_str(" second part");
    s.push_str(" third part");

    @dbg(s);  // prints: first part second part third part

    0
}
```

## Clearing a String

`clear` empties a string but keeps the allocated memory for reuse:

```gruel
fn main() -> i32 {
    let mut s = String::new();
    s.push_str("temporary");
    s.clear();
    s.push_str("reused");

    @dbg(s);  // prints: reused

    0
}
```
