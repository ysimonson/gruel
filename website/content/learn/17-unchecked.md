+++
title = "Unchecked Code and Raw Pointers"
weight = 18
template = "learn/page.html"
+++

# Unchecked Code and Raw Pointers

Gruel's safety guarantees hold everywhere by default. For low-level work — implementing data structures, calling OS interfaces, or writing FFI bindings — you can opt out of those guarantees inside `checked` blocks.

The name `checked` reflects that *you* are taking responsibility for checking the invariants the compiler normally verifies.

## Checked Blocks

`checked { ... }` is a block expression that allows raw-pointer and syscall operations inside it. Its value is the value of the block:

```gruel
fn main() -> i32 {
    let mut value: i32 = 10;

    let v: i32 = checked {
        let p: MutPtr(i32) = MutPtr(i32)::from(&mut value);
        p.write(42);
        p.read()
    };

    @dbg(v);   // 42
    value
}
```

Code outside `checked` blocks is completely unaffected — all of Gruel's safety rules still apply there.

## Raw Pointer Types

Two raw pointer types are available, parameterized by the pointee:

```
Ptr(T)      // read-only pointer to T
MutPtr(T)   // read-write pointer to T
```

## Getting a Pointer from a Value

Construct a pointer from a reference using the type's `from` associated function. Combine it with the `&` and `&mut` operators from [Borrow and Inout](@/learn/09-borrow-and-inout.md):

```gruel
fn main() -> i32 {
    let x: i32 = 7;
    let mut y: i32 = 0;

    let result = checked {
        let p: Ptr(i32) = Ptr(i32)::from(&x);
        let q: MutPtr(i32) = MutPtr(i32)::from(&mut y);
        q.write(p.read() * 6);
        q.read()
    };

    @dbg(result);  // 42
    result
}
```

## Pointer Methods

Once you have a pointer, the operations are methods on the pointer type:

| Method | Available on | Description |
|--------|--------------|-------------|
| `p.read()` | `Ptr(T)`, `MutPtr(T)` | Load the value at `p` |
| `p.write(v)` | `MutPtr(T)` | Store `v` at `p` |
| `p.offset(n)` | `Ptr(T)`, `MutPtr(T)` | Advance by `n` *elements* (not bytes) |
| `p.is_null()` | `Ptr(T)`, `MutPtr(T)` | Test whether `p` is null |
| `p.to_int()` | `Ptr(T)`, `MutPtr(T)` | Pointer's address as `u64` |

Constructors live on the type itself:

| Constructor | Description |
|-------------|-------------|
| `Ptr(T)::null()` / `MutPtr(T)::null()` | Null pointer |
| `Ptr(T)::from(r)` / `MutPtr(T)::from(r)` | Wrap a `Ref(T)` / `MutRef(T)` |
| `Ptr(T)::from_int(addr)` / `MutPtr(T)::from_int(addr)` | Reinterpret an integer address |

## Pointer Arithmetic

`p.offset(n)` advances by `n` *elements*, not bytes — the pointee type determines the stride:

```gruel
fn main() -> i32 {
    let arr = [1, 2, 3, 4, 5];

    let third = checked {
        let base: Ptr(i32) = Ptr(i32)::from(&arr[0]);
        let p = base.offset(2);   // points at arr[2]
        p.read()
    };

    third  // 3
}
```

## Syscalls

`@syscall` makes a direct OS system call. The first argument is the syscall number; the rest are arguments (up to six). It returns `i64`:

```gruel
fn main() -> i32 {
    checked {
        // syscall 1 = write(fd, buf, len) on Linux
        @syscall(1, 1, 0, 0);
    }
    0
}
```

Syscall numbers and ABI conventions are platform-specific. Use `@target_os()` and `@target_arch()` to branch between platforms.

## Unchecked Functions

Mark a function `unchecked` to signal that it performs low-level operations. Callers must invoke it from inside a `checked` block:

```gruel
unchecked fn dangerous_op() -> i32 { 42 }

fn main() -> i32 {
    checked { dangerous_op() }
}
```

Calling an `unchecked` function outside a `checked` block is a compile-time error.

## What You Are Responsible For

Inside `checked` blocks the compiler does not verify:

- That pointers are non-null before dereferencing
- That pointers point to valid, correctly-typed memory
- That there are no aliasing violations (two `MutPtr` to the same location)
- That pointers don't outlive the value they came from

These are your responsibility. Outside `checked` blocks, Gruel's normal guarantees are fully in force.

## When to Use Checked Blocks

Checked blocks are for implementing low-level primitives — allocators, collections, OS wrappers, FFI. Most application code should never need them. If you find yourself reaching for `checked` in business logic, consider whether there's a safe abstraction that already does what you need.
