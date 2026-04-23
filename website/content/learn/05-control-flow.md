+++
title = "Control Flow"
weight = 5
template = "learn/page.html"
+++

# Control Flow

Gruel has three main control flow constructs: `if`, `while`, and `match`.

## If Expressions

`if` is an expression, not a statement—it returns a value:

```gruel
fn max(a: i32, b: i32) -> i32 {
    if a > b { a } else { b }
}

fn main() -> i32 {
    let bigger = max(10, 20);
    @dbg(bigger);  // prints: 20
    bigger
}
```

Because `if` is an expression, you can use it anywhere a value is expected:

```gruel
fn main() -> i32 {
    let x = 5;
    let description = if x > 0 { 1 } else { 0 };
    @dbg(description);  // prints: 1
    0
}
```

Both branches must have the same type.

## While Loops

```gruel
fn main() -> i32 {
    let mut sum = 0;
    let mut i = 1;

    while i <= 10 {
        sum = sum + i;
        i = i + 1;
    }

    @dbg(sum);  // prints: 55 (1+2+...+10)
    sum
}
```

Use `break` to exit a loop early, and `continue` to skip the rest of the current iteration:

```gruel
fn main() -> i32 {
    // break: exit as soon as we find 5
    let mut i = 0;
    while true {
        i = i + 1;
        if i == 5 { break }
    }
    @dbg(i);  // prints: 5

    // continue: skip odd numbers, sum even ones up to 10
    let mut sum = 0;
    let mut j = 0;
    while j < 10 {
        j = j + 1;
        if j % 2 != 0 { continue }
        sum = sum + j;
    }
    @dbg(sum);  // prints: 30  (2+4+6+8+10)

    0
}
```

`break` and `continue` only affect the innermost loop.

## Infinite Loops

Use `loop` for an unconditional loop. It runs forever unless `break` is used:

```gruel
fn main() -> i32 {
    let mut count = 0;
    loop {
        count = count + 1;
        if count == 3 { break }
    }
    @dbg(count);  // prints: 3
    count
}
```

`loop` is equivalent to `while true` but more clearly signals intent.

## Match Expressions

For multi-way branching, use `match`:

```gruel
fn day_type(day: i32) -> i32 {
    // 0 = weekend, 1 = weekday
    match day {
        0 => 0,  // Sunday
        6 => 0,  // Saturday
        _ => 1,  // Everything else
    }
}

fn main() -> i32 {
    @dbg(day_type(0));  // prints: 0 (weekend)
    @dbg(day_type(3));  // prints: 1 (weekday)
    0
}
```

The `_` is a wildcard that matches anything. Match expressions must be exhaustive—every possible value must be handled.

## Example: FizzBuzz

Here's a classic example combining everything:

```gruel
fn fizzbuzz(n: i32) -> i32 {
    let div_by_3 = n % 3 == 0;
    let div_by_5 = n % 5 == 0;

    if div_by_3 && div_by_5 {
        3  // FizzBuzz
    } else if div_by_3 {
        1  // Fizz
    } else if div_by_5 {
        2  // Buzz
    } else {
        n
    }
}

fn main() -> i32 {
    let mut i = 1;
    while i <= 15 {
        @dbg(fizzbuzz(i));
        i = i + 1;
    }
    0
}
```
