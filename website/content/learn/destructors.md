+++
title = "Destructors"
weight = 13
template = "learn/page.html"
+++

# Destructors

When a value goes out of scope, Gruel cleans it up automatically. This page covers how and when that cleanup happens, and how to write your own.

## Trivially Droppable Types

Most types need no cleanup at all. Integers, booleans, unit, enums — these are *trivially droppable*. When they go out of scope, nothing happens:

```gruel
fn main() -> i32 {
    let x = 42;
    let flag = true;
    0
}  // x and flag go out of scope — no cleanup needed
```

A struct is trivially droppable if all its fields are trivially droppable:

```gruel
struct Point { x: i32, y: i32 }

fn main() -> i32 {
    let p = Point { x: 1, y: 2 };
    p.x  // p goes out of scope — no cleanup needed
}
```

## Types with Destructors

Some types need cleanup. `String` allocates heap memory, so dropping a String frees that memory:

```gruel
fn main() -> i32 {
    let s = "hello";
    @dbg(s);
    0
}  // s is dropped here — its memory is freed
```

If a struct contains a field with a destructor, that struct also has a destructor:

```gruel
struct Message {
    text: String,
    priority: i32,
}

fn main() -> i32 {
    let msg = Message { text: "urgent", priority: 1 };
    @dbg(msg.priority);
    0
}  // msg is dropped — msg.text (a String) is freed
```

## Drop Order

When multiple values go out of scope at the same point, they are dropped in **reverse declaration order** — last declared, first dropped:

```gruel
struct Data { value: i32 }

drop fn Data(self) {
    @dbg(self.value);
}

fn main() -> i32 {
    let a = Data { value: 1 };  // declared first
    let b = Data { value: 2 };  // declared second
    0
}  // prints 2, then 1
```

This LIFO order ensures that values declared later — which may depend on earlier values — are cleaned up first.

Within a struct, fields are dropped in **declaration order** (first declared, first dropped):

```gruel
struct Pair {
    first: String,   // dropped first
    second: String,  // dropped second
}
```

## When Drops Happen

Drops are inserted automatically at several points:

**End of a block scope:**

```gruel
fn main() -> i32 {
    let outer = "outer";
    {
        let inner = "inner";
        @dbg(inner);
    }  // inner is dropped here

    @dbg(outer);
    0
}  // outer is dropped here
```

**Before a return statement** — all live values in enclosing scopes are dropped:

```gruel
fn example(condition: bool) -> i32 {
    let a = "first";
    if condition {
        let b = "second";
        return 42;  // b dropped, then a dropped, then return
    }
    let c = "third";
    0  // c dropped, then a dropped
}
```

**Before a break statement** — values declared inside the loop are dropped:

```gruel
fn main() -> i32 {
    let mut i = 0;
    while i < 10 {
        let s = "temporary";
        if i == 3 {
            break;  // s is dropped before breaking
        }
        i = i + 1;
    }  // s is dropped at end of each iteration
    i
}
```

Each branch of a conditional independently drops its own values.

## Moved Values Are Not Dropped

If a value is moved (passed to a function or assigned to another variable), it is **not** dropped at its original scope. It will be dropped at its new location:

```gruel
fn sink(s: String) -> i32 {
    @dbg(s);
    0
}  // s is dropped here (owned by sink)

fn main() -> i32 {
    let s = "hello";
    sink(s)
}  // s is NOT dropped here (it was moved into sink)
```

## Custom Destructors

Define a destructor with `drop fn` to run custom cleanup logic when a value is dropped:

```gruel
struct FileHandle {
    fd: i32,
}

drop fn FileHandle(self) {
    @dbg(self.fd);  // cleanup logic here
}

fn main() -> i32 {
    let f = FileHandle { fd: 3 };
    0
}  // prints: 3
```

A destructor must be declared at the top level (not inside an `impl` block), take exactly one parameter named `self`, and return nothing. Each type can have at most one destructor.

Linear types (`linear struct`) cannot have destructors. A linear value must be explicitly consumed — the compiler rejects any code where one reaches scope exit unconsumed, so a destructor would never run.

## Destructor Composition

When a value with a custom destructor is dropped, the custom destructor runs **first**, then fields with destructors are dropped automatically:

```gruel
struct Buffer {
    data: String,
    size: i32,
}

drop fn Buffer(self) {
    @dbg(self.size);
    // After this runs, self.data (a String) is dropped automatically
}

fn main() -> i32 {
    let buf = Buffer { data: "contents", size: 8 };
    0
}  // Buffer destructor runs (prints 8), then data's String memory is freed
```

You don't need to manually drop fields — the compiler handles it.

## Function Parameters

Parameters passed by value are owned by the callee. If the parameter isn't moved away, it's dropped when the function returns:

```gruel
struct Data { value: i32 }

drop fn Data(self) {
    @dbg(self.value);
}

fn inspect(d: Data) -> i32 {
    0
}  // d is dropped here (prints 42)

fn main() -> i32 {
    let d = Data { value: 42 };
    inspect(d)  // ownership transfers to inspect
}  // d is NOT dropped here
```

Borrow and inout parameters are not owned by the callee and are never dropped there.
