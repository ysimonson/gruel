+++
title = "Functions"
weight = 1
template = "spec/page.html"
+++

# Functions

{{ rule(id="6.1:1", cat="normative") }}

A function is defined using the `fn` keyword.

{{ rule(id="6.1:2", cat="normative") }}

```ebnf
function = "fn" IDENT "(" [ params ] ")" [ "->" type ] "{" block "}" ;
params = param { "," param } ;
param = IDENT ":" type ;
```

## Function Signature

{{ rule(id="6.1:3", cat="legality-rule") }}

Parameters **MUST** have explicit type annotations.

{{ rule(id="6.1:4", cat="legality-rule") }}

If a return type is specified, the function body **MUST** produce a value of that type.

{{ rule(id="6.1:5", cat="normative") }}

If no return type is specified, the function returns `()`.

{{ rule(id="6.1:6", cat="normative") }}

```gruel
fn add(x: i32, y: i32) -> i32 {
    x + y
}

fn do_nothing() {
    // implicitly returns ()
}
```

## Reference Parameters: `Ref(T)` and `MutRef(T)` (ADR-0076)

> **Historical note (informative):** Through ADR-0062 the language
> exposed parameter modes `borrow` / `inout` for non-owning parameter
> passing. ADR-0076 retired those keywords; `Ref(T)` and `MutRef(T)`
> (constructed at call sites with `&x` / `&mut x`) are now the only
> form. Paragraph IDs 6.1:14..31 are kept stable so prior spec test
> references continue to resolve.

{{ rule(id="6.1:14", cat="normative") }}

A parameter declared with type `MutRef(T)` is passed by reference and may be mutated by the callee. Changes made through a `MutRef(T)` parameter are visible to the caller after the call returns.

{{ rule(id="6.1:15", cat="syntax") }}

```ebnf
param = [ "comptime" ] IDENT ":" type ;
type  = …                                      (* see 3.1, including
                                                 `Ref ( T )` and
                                                 `MutRef ( T )` *) ;
```

{{ rule(id="6.1:16", cat="legality-rule") }}

A call site argument supplied to a `MutRef(T)` parameter **MUST** be a `&mut x` expression (6.1:35). Argument expressions of any other shape are rejected as a type mismatch.

{{ rule(id="6.1:17", cat="legality-rule") }}

The operand of `&mut` **MUST** be an lvalue (a variable, field access, or array index expression).

{{ rule(id="6.1:18", cat="dynamic-semantics") }}

When a function is called with a `&mut x` argument:
1. The address of `x` is passed to the callee.
2. The callee reads and writes through this address.
3. After the call returns, the original variable holds the updated value.

{{ rule(id="6.1:19", cat="example") }}

```gruel
fn increment(x: MutRef(i32)) {
    x = x + 1;     // bare-name write-through (6.1:43)
}

fn main() -> i32 {
    let mut n = 10;
    increment(&mut n);
    n  // 11
}
```

{{ rule(id="6.1:20", cat="legality-rule") }}

A single function call **MUST NOT** target the same lvalue with multiple `&mut` references. This prevents aliasing of mutable references within a single call.

{{ rule(id="6.1:21", cat="example") }}

```gruel
fn swap(a: MutRef(i32), b: MutRef(i32)) {
    let tmp = a;
    a = b;
    b = tmp;
}

fn main() -> i32 {
    let mut x = 1;
    swap(&mut x, &mut x);  // error: cannot alias `x` for two MutRefs
    0
}
```

{{ rule(id="6.1:22", cat="normative") }}

A parameter declared with type `Ref(T)` is passed by reference for read-only access. The callee cannot mutate through a `Ref(T)` parameter, and the value is not consumed (ownership is not transferred).

{{ rule(id="6.1:23", cat="legality-rule") }}

A call-site argument supplied to a `Ref(T)` parameter **MUST** be a `&x` expression (6.1:35). Argument expressions of any other shape are rejected as a type mismatch.

{{ rule(id="6.1:24", cat="legality-rule") }}

The body of a function **MUST NOT** mutate through a `Ref(T)` parameter. This includes:
- Bare-name assignment to the parameter (6.1:44).
- Assignment to fields or array elements reached through the parameter.

{{ rule(id="6.1:25", cat="legality-rule") }}

The body of a function **MUST NOT** move out of a `Ref(T)` parameter. A reference cannot be returned, stored in a struct field, or passed to a function expecting an owned value.

{{ rule(id="6.1:26", cat="dynamic-semantics") }}

When a function is called with a `&x` argument:
1. The address of `x` is passed to the callee.
2. The callee reads through this address.
3. After the call returns, the original variable is unchanged and still valid.

{{ rule(id="6.1:27", cat="example") }}

```gruel
struct Point { x: i32, y: i32 }

fn sum_coords(p: Ref(Point)) -> i32 {
    p.x + p.y
}

fn main() -> i32 {
    let p = Point { x: 10, y: 32 };
    let result = sum_coords(&p);
    result + p.x - p.x  // p is still valid after the call
}
```

{{ rule(id="6.1:28", cat="normative") }}

Multiple `Ref(T)` parameters **MAY** target the same lvalue at one call site. Unlike `MutRef(T)`, immutable references are shared read-only access.

{{ rule(id="6.1:29", cat="example") }}

```gruel
fn sum_both(a: Ref(i32), b: Ref(i32)) -> i32 {
    a + b
}

fn main() -> i32 {
    let x = 21;
    sum_both(&x, &x)  // OK: multiple immutable refs of same lvalue
}
```

{{ rule(id="6.1:30", cat="legality-rule") }}

A single function call **MUST NOT** target the same lvalue with both a `&` reference and a `&mut` reference. This enforces the law of exclusivity: either one `MutRef` or any number of `Ref`s, but never both simultaneously.

{{ rule(id="6.1:31", cat="example") }}

```gruel
fn mixed(a: Ref(i32), b: MutRef(i32)) {
    b = a + 1;
}

fn main() -> i32 {
    let mut x = 41;
    mixed(&x, &mut x);  // error: cannot alias `x` as both Ref and MutRef
    0
}
```

## Reference Types (ADR-0062, ADR-0076)

{{ rule(id="6.1:34", cat="normative") }}

A `Ref(T)` value is a scope-bound immutable reference to a value of type `T`. A `MutRef(T)` value is a scope-bound exclusive mutable reference to a value of type `T`. References do not transfer ownership and **MUST NOT** escape the function in which they are constructed.

{{ rule(id="6.1:35", cat="syntax") }}

```ebnf
ref_expr = "&" expr ;
mut_ref_expr = "&" "mut" expr ;
```

{{ rule(id="6.1:36", cat="legality-rule") }}

The operand of `&` or `&mut` **MUST** be an lvalue (a variable, field access, or array index expression).

{{ rule(id="6.1:37", cat="dynamic-semantics") }}

`&x` evaluates to a `Ref(T)` whose target is the storage of `x`. `&mut x` evaluates to a `MutRef(T)` whose target is the storage of `x`. Both forms produce a single pointer-sized value.

{{ rule(id="6.1:38", cat="example") }}

```gruel
fn read(r: Ref(i32)) -> i32 {
    42
}

fn main() -> i32 {
    let x: i32 = 7;
    read(&x)
}
```

{{ rule(id="6.1:39", cat="legality-rule") }}

Within a single function call, the same lvalue **MUST NOT** be the target of multiple `&mut` references, and **MUST NOT** be the target of both a `&` reference and a `&mut` reference (cf. 6.1:20, 6.1:30). The trigger is the type of the constructed reference.

{{ rule(id="6.1:40", cat="legality-rule") }}

The operand of `&mut` **MUST** be bound by `let mut` (or be a field/index of such a binding). Constructing `&mut x` of an immutable binding is a compile-time error.

{{ rule(id="6.1:41", cat="legality-rule") }}

A function **MUST NOT** return a value whose type is `Ref(T)` or `MutRef(T)`. References are scope-bound and cannot outlive the function in which they are constructed.

{{ rule(id="6.1:42", cat="syntax") }}

Method receivers (ADR-0076) follow the parameter form. The annotation is required only when not by-value:

```ebnf
self_param = "self" [ ":" ( "Self" | "Ref" "(" "Self" ")" | "MutRef" "(" "Self" ")" ) ] ;
```

### Bare-Name Write-Through (ADR-0076)

{{ rule(id="6.1:43", cat="dynamic-semantics") }}

For any binding `r` whose declared type is `MutRef(T)` — whether a parameter (`fn set(r: MutRef(T), ...)`) or a local (`let r: MutRef(T) = &mut x;`) — the assignment `r = e` evaluates `e` to a value of type `T` and stores it at the place referenced by `r`. The binding `r` itself is never rebound. There is no separate dereference operator; this is the only write-through form.

{{ rule(id="6.1:44", cat="legality-rule") }}

For a binding whose declared type is `Ref(T)`, the assignment `r = e` is a compile-time error. `Ref(T)` is read-only.

{{ rule(id="6.1:45", cat="example") }}

```gruel
// Parameter-position write-through.
fn set(r: MutRef(i32), v: i32) {
    r = v;            // stores v through r
}

// Local-position write-through.
fn local() -> i32 {
    let mut x: i32 = 0;
    let r: MutRef(i32) = &mut x;
    r = 42;           // stores 42 through r
    x                  // 42
}
```

{{ rule(id="6.1:46", cat="dynamic-semantics") }}

`e` is evaluated before the address of the pointee is taken. References remain scope-bound (6.1:41); a `let r: MutRef(T) = &mut x;` binding obeys the same non-escape rules as a `MutRef`-typed parameter.

## Parameter Immutability

{{ rule(id="6.1:32", cat="legality-rule") }}

A parameter whose declared type is not `MutRef(T)` is immutable within the function body. Assigning to such a parameter or modifying its fields is a compile-time error.

{{ rule(id="6.1:33", cat="example") }}

```gruel
fn bad(x: i32) {
    x = 5;  // error: cannot assign to immutable parameter 'x'
}

struct Point { x: i32, y: i32 }

fn also_bad(p: Point) {
    p.x = 10;  // error: cannot assign to immutable parameter 'p'
}
```


## Entry Point

{{ rule(id="6.1:7", cat="legality-rule") }}

A program **MUST** have a function named `main`.

{{ rule(id="6.1:8", cat="legality-rule") }}

The `main` function **MUST** return either `i32` or `()`. When it returns `i32`, that value becomes the program's exit code. When it returns `()`, the exit code is 0.

{{ rule(id="6.1:9") }}

```gruel
fn main() -> i32 {
    42  // exit code is 42
}
```

## Recursion

{{ rule(id="6.1:10", cat="normative") }}

Functions **MAY** call themselves recursively.

{{ rule(id="6.1:11") }}

```gruel
fn factorial(n: i32) -> i32 {
    if n <= 1 { 1 }
    else { n * factorial(n - 1) }
}

fn main() -> i32 {
    factorial(5)  // 120
}
```

## Function Visibility

{{ rule(id="6.1:12", cat="normative") }}

Functions **MAY** call any function defined in the same module, regardless of definition order.

{{ rule(id="6.1:13") }}

```gruel
fn main() -> i32 {
    helper()  // can call function defined below
}

fn helper() -> i32 {
    42
}
```
