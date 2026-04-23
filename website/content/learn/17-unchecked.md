+++
title = "Unchecked Code and Raw Pointers"
weight = 18
template = "learn/page.html"
+++

# Unchecked Code and Raw Pointers

Gruel's safety guarantees hold everywhere by default. For low-level work — implementing data structures, calling OS interfaces, or writing FFI bindings — you can opt out of those guarantees inside `checked` blocks.

The name `checked` reflects that *you* are taking responsibility for checking the invariants the compiler normally verifies.

## Checked Blocks

Wrap low-level operations in a `checked` block:

```gruel
fn example() -> i32 {
    // Normal safe code here

    checked {
        // Raw pointer operations permitted here
    }

    // Normal safe code resumes
    0
}
```

Code outside `checked` blocks is completely unaffected — all of Gruel's safety rules still apply there.

## Raw Pointer Types

Two raw pointer types are available inside `checked` blocks:

```gruel
ptr const T   // read-only pointer to T
ptr mut T     // read-write pointer to T
```

## Getting Pointers from Values

Use `@raw` and `@raw_mut` to obtain pointers from existing values:

```gruel
fn inspect(borrow s: String) {
    checked {
        let p: ptr const String = @raw(s);
        // p is valid while s is in scope
    }
}

fn modify(inout s: String) {
    checked {
        let p: ptr mut String = @raw_mut(s);
    }
}
```

| Intrinsic | Input | Result |
|-----------|-------|--------|
| `@raw(x)` | `borrow T` or owned `T` | `ptr const T` |
| `@raw_mut(x)` | `inout T` or owned `T` | `ptr mut T` |

## Reading and Writing Through Pointers

```gruel
fn main() -> i32 {
    let mut value: i32 = 10;
    checked {
        let p: ptr mut i32 = @raw_mut(value);

        // Write through pointer
        @ptr_write(p, 42);

        // Read through pointer
        let v: i32 = @ptr_read(p);
        @dbg(v);  // prints: 42
    }
    value
}
```

## Pointer Arithmetic

`@ptr_offset` advances a pointer by a number of *elements* (not bytes):

```gruel
fn sum_array(p: ptr const i32, len: u64) -> i32 {
    let mut total = 0;
    let mut i: u64 = 0;
    checked {
        while i < len {
            let elem_ptr = @ptr_offset(p, @intCast(i));
            total = total + @ptr_read(elem_ptr);
            i = i + 1;
        }
    }
    total
}

fn main() -> i32 {
    let arr = [1, 2, 3, 4, 5];
    checked {
        let p: ptr const i32 = @raw(arr);
        sum_array(p, 5)
    }
}
```

## All Pointer Intrinsics

| Intrinsic | Signature | Description |
|-----------|-----------|-------------|
| `@ptr_read(p)` | `(ptr const T) -> T` | Read value at pointer |
| `@ptr_write(p, v)` | `(ptr mut T, T) -> ()` | Write value at pointer |
| `@ptr_offset(p, n)` | `(ptr T, i64) -> ptr T` | Advance by n elements |
| `@ptr_to_int(p)` | `(ptr T) -> u64` | Pointer as integer address |
| `@int_to_ptr(n)` | `(u64) -> ptr mut T` | Integer address as pointer |
| `@null_ptr()` | `() -> ptr const T` | Null pointer |
| `@is_null(p)` | `(ptr T) -> bool` | Test for null |
| `@ptr_copy(dst, src, n)` | `(ptr mut T, ptr const T, u64) -> ()` | Copy n elements |
| `@raw(x)` | `borrow T` or `T` | `ptr const T` from value |
| `@raw_mut(x)` | `inout T` or `T` | `ptr mut T` from value |

## Syscalls

`@syscall` makes a direct OS system call. The first argument is the syscall number; the rest are arguments (up to six). It returns `i64`:

```gruel
fn write_stdout(borrow s: String) {
    checked {
        let ptr = @raw(s);
        // syscall 1 = write(fd, buf, len) on Linux
        @syscall(1, 1, @ptr_to_int(ptr), s.len());
    }
}

fn main() -> i32 {
    write_stdout(borrow "hello from syscall\n");
    0
}
```

Syscall numbers are platform-specific. Use `@target_os()` to branch between Linux and macOS if needed.

## Unchecked Functions

Mark a function `unchecked` to signal that it performs low-level operations. Callers must call it from inside a `checked` block:

```gruel
unchecked fn raw_copy(dst: ptr mut i32, src: ptr const i32, n: u64) {
    checked {
        @ptr_copy(dst, src, n);
    }
}

fn use_it(inout dst: [i32; 4], borrow src: [i32; 4]) {
    checked {
        raw_copy(@raw_mut(dst), @raw(src), 4);
    }
}
```

## What You Are Responsible For

Inside `checked` blocks the compiler does not verify:

- That pointers are non-null before dereferencing
- That pointers point to valid, correctly-typed memory
- That there are no aliasing violations (two `ptr mut` to the same location)
- That pointers from `@raw`/`@raw_mut` don't outlive the value they came from

These are your responsibility. Outside `checked` blocks, Gruel's normal guarantees are fully in force.

## When to Use Checked Blocks

Checked blocks are for implementing low-level primitives — allocators, collections, OS wrappers, FFI. Most application code should never need them. If you find yourself reaching for `checked` in business logic, consider whether there's a safe abstraction that already does what you need.
