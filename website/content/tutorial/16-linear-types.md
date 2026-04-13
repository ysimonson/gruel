+++
title = "Linear Types"
weight = 16
template = "tutorial/page.html"
+++

# Linear Types

Gruel's type system has three levels of ownership discipline:

| Annotation | Behaviour |
|------------|-----------|
| *(none)* | Affine — used at most once; can be silently dropped |
| `linear` | Linear — must be consumed exactly once |
| `@copy` | Copy — implicitly duplicated on use |

You've already seen affine structs (move semantics) and `@copy` structs. This page covers `linear` types and the related `@handle` directive.

## Linear Types Must Be Consumed

Mark a struct with `linear` to require that every value of that type is explicitly consumed — it is a compile error to let one go out of scope without being used:

```gruel
linear struct Token { value: i32 }

fn use_token(t: Token) -> i32 { t.value }

fn main() -> i32 {
    let t = Token { value: 42 };
    use_token(t)  // t is consumed here — OK
}
```

If you create a linear value and never consume it, the compiler rejects the program:

```gruel
linear struct Token { value: i32 }

fn main() -> i32 {
    let t = Token { value: 1 };
    // ERROR: linear value `t` dropped without being consumed
    0
}
```

This is useful for types that represent resources requiring explicit action — tokens that must be redeemed, handles that must be closed, results that must be checked.

## Consuming a Linear Value

A linear value is consumed by any of these:

- Passing it to a function (by value)
- Returning it from a function
- Accessing a field (field access counts as a move)

```gruel
linear struct Ticket { id: i32 }

fn redeem(t: Ticket) -> i32 { t.id }

fn main() -> i32 {
    let t = Ticket { id: 7 };
    redeem(t)
}
```

## Chaining Through Functions

You can transform a linear value by passing it to a function that returns a new one. The chain preserves the must-consume guarantee:

```gruel
linear struct Value { n: i32 }

fn double(v: Value) -> Value {
    Value { n: v.n * 2 }
}

fn finish(v: Value) -> i32 { v.n }

fn main() -> i32 {
    let v = Value { n: 21 };
    let v2 = double(v);
    finish(v2)   // prints 42
}
```

## All Branches Must Consume

If a linear value might go unconsumed in any branch, the compiler errors. You must consume it in every branch:

```gruel
linear struct Permit { id: i32 }

fn use_it(p: Permit) -> i32 { p.id }

fn main() -> i32 {
    let p = Permit { id: 42 };
    let cond = true;
    if cond {
        use_it(p)   // consumed in true branch
    } else {
        use_it(p)   // consumed in false branch — both required
    }
}
```

## Linear Types Cannot Be `@copy`

Allowing implicit copies would defeat the purpose of linear types — you could copy before dropping to avoid the consume requirement. The compiler rejects this combination:

```gruel
@copy
linear struct Bad { value: i32 }
// ERROR: linear type cannot be marked @copy
```

## Explicit Duplication with `@handle`

Sometimes you legitimately need two handles to the same logical resource — for example, forking a value for two code paths. The `@handle` directive enables this with explicit syntax.

A type marked `@handle` must define a `handle` method that produces a new owned value:

```gruel
@handle
struct Counter {
    count: i32,

    fn handle(self) -> Counter {
        Counter { count: self.count }
    }
}

fn main() -> i32 {
    let a = Counter { count: 42 };
    let b = a.handle();  // explicit duplication — cost is visible
    b.count
}
```

Unlike `@copy`, duplication is never implicit. You must call `.handle()`, making the cost visible at every use site.

## `@handle` with `linear`

You can combine `@handle` and `linear`. This gives you explicit duplication while still requiring every handle to be consumed:

```gruel
@handle
linear struct Task {
    id: i32,

    fn handle(self) -> Task {
        Task { id: self.id }
    }
}

fn run(t: Task) -> i32 { t.id }

fn main() -> i32 {
    let t = Task { id: 42 };
    let t2 = t.handle();  // fork — both handles must be consumed
    run(t2)               // consume t2
    // ERROR if t is not also consumed — add: run(t)
}
```

## When to Use Linear Types

Use `linear` when:

- A value represents a resource that **must** be explicitly finalized (a connection that must be committed or rolled back, a lock that must be released)
- You want the type system to guarantee a result is checked before it is discarded
- You're encoding a protocol where skipping a step should be a compile error

For most types, affine semantics (the default) are sufficient — values can be dropped without any action.
