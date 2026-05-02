+++
title = "Enums"
weight = 8
template = "learn/page.html"
+++

# Enums

Enums define types with a fixed set of possible values, called variants.

## Defining Enums

```gruel
enum Color {
    Red,
    Green,
    Blue,
}

fn main() -> i32 {
    let c = Color::Green;
    0
}
```

Variants are accessed with the `::` syntax: `EnumName::VariantName`.

## Matching on Enums

Use `match` to handle each variant:

```gruel
enum Color {
    Red,
    Green,
    Blue,
}

fn color_value(c: Color) -> i32 {
    match c {
        Color::Red => 1,
        Color::Green => 2,
        Color::Blue => 3,
    }
}

fn main() -> i32 {
    @dbg(color_value(Color::Red));    // prints: 1
    @dbg(color_value(Color::Green));  // prints: 2
    @dbg(color_value(Color::Blue));   // prints: 3
    0
}
```

## Exhaustive Matching

Match expressions on enums must be exhaustive—you must handle all variants:

```gruel
enum Direction {
    North,
    South,
    East,
    West,
}

fn to_degrees(d: Direction) -> i32 {
    match d {
        Direction::North => 0,
        Direction::East => 90,
        Direction::South => 180,
        Direction::West => 270,
    }
}
```

If you forget a variant, the compiler will tell you:

```gruel
fn to_degrees(d: Direction) -> i32 {
    match d {
        Direction::North => 0,
        Direction::East => 90,
        // Error: non-exhaustive match, missing South and West
    }
}
```

Use `_` as a wildcard to match any remaining variants:

```gruel
fn is_north(d: Direction) -> bool {
    match d {
        Direction::North => true,
        _ => false,
    }
}
```

## Data Variants

Variants can carry associated data. Declare the field types in parentheses after the variant name:

```gruel
enum IntOption {
    Some(i32),
    None,
}

fn main() -> i32 {
    let x = IntOption::Some(42);
    let y = IntOption::None;
    0
}
```

An enum can mix unit variants (no data) and data variants freely.

### Matching Data Variants

Use binding patterns to extract the data from a variant:

```gruel
enum IntOption {
    Some(i32),
    None,
}

fn unwrap_or(opt: IntOption, default: i32) -> i32 {
    match opt {
        IntOption::Some(v) => v,
        IntOption::None => default,
    }
}

fn main() -> i32 {
    let x = IntOption::Some(42);
    let y = IntOption::None;

    @dbg(unwrap_or(x, 0));  // prints: 42
    @dbg(unwrap_or(y, 0));  // prints: 0
    0
}
```

Use `_` to discard a field you don't need:

```gruel
enum Result {
    Ok(i32),
    Err(i32),
}

fn is_ok(r: Result) -> bool {
    match r {
        Result::Ok(_) => true,
        Result::Err(_) => false,
    }
}
```

Variants can carry multiple fields:

```gruel
enum Event {
    Click(i32, i32),
    KeyPress(i32),
    Quit,
}

fn describe(e: Event) -> i32 {
    match e {
        Event::Click(x, y) => x + y,
        Event::KeyPress(code) => code,
        Event::Quit => 0,
    }
}
```

## Struct Variants

Variants can also carry named fields, like a struct:

```gruel
enum Shape {
    Circle { radius: i32 },
    Rectangle { width: i32, height: i32 },
    Point,
}

fn main() -> i32 {
    let s = Shape::Circle { radius: 5 };
    0
}
```

Field init shorthand works here too—if a variable has the same name as a field, you can omit the `: value` part:

```gruel
fn make_circle(radius: i32) -> Shape {
    Shape::Circle { radius }
}
```

### Matching Struct Variants

Use field bindings in braces to destructure struct variants. All fields must be listed:

```gruel
enum Shape {
    Circle { radius: i32 },
    Rectangle { width: i32, height: i32 },
    Point,
}

fn area(s: Shape) -> i32 {
    match s {
        Shape::Circle { radius } => radius * radius,
        Shape::Rectangle { width, height } => width * height,
        Shape::Point => 0,
    }
}

fn main() -> i32 {
    @dbg(area(Shape::Circle { radius: 5 }));            // prints: 25
    @dbg(area(Shape::Rectangle { width: 3, height: 4 })); // prints: 12
    @dbg(area(Shape::Point));                            // prints: 0
    0
}
```

You can rebind a field to a different name with `field: new_name`, or discard it with `field: _`:

```gruel
fn get_width(s: Shape) -> i32 {
    match s {
        Shape::Rectangle { width: w, height: _ } => w,
        _ => 0,
    }
}
```

## Enums in Structs

Enums can be fields in structs:

```gruel
enum Status {
    Pending,
    Active,
    Completed,
}

struct Task {
    id: i32,
    status: Status,
}

fn is_done(task: Task) -> bool {
    match task.status {
        Status::Completed => true,
        _ => false,
    }
}

fn main() -> i32 {
    let task = Task {
        id: 1,
        status: Status::Active,
    };

    @dbg(is_done(task));  // prints: false
    0
}
```

## Generic Enums with Comptime

Just like structs, you can create generic enum types using comptime functions that return `type`. This uses anonymous enum syntax:

```gruel
fn Option(comptime T: type) -> type {
    enum {
        Some(T),
        None,
    }
}

fn main() -> i32 {
    let x: Option(i32) = Option(i32)::Some(42);
    let y: Option(i32) = Option(i32)::None;

    match x {
        Option(i32)::Some(v) => @dbg(v),  // prints: 42
        Option(i32)::None => @dbg(0),
    };
    0
}
```

You can add methods to anonymous enums using `Self` to refer to the type being defined:

```gruel
fn Option(comptime T: type) -> type {
    enum {
        Some(T),
        None,

        fn unwrap_or(self, default: T) -> T {
            match self {
                Self::Some(v) => v,
                Self::None => default,
            }
        }
    }
}

fn main() -> i32 {
    let x = Option(i32)::Some(42);
    let y = Option(i32)::None;

    @dbg(x.unwrap_or(0));  // prints: 42
    @dbg(y.unwrap_or(0));  // prints: 0
    0
}
```

This is the idiomatic way to build reusable sum types in Gruel. See [Comptime and Generics](/learn/14-comptime/) for more on comptime.

## Example: Simple State Machine

```gruel
enum TrafficLight {
    Red,
    Yellow,
    Green,
}

fn next_light(current: TrafficLight) -> TrafficLight {
    match current {
        TrafficLight::Red => TrafficLight::Green,
        TrafficLight::Green => TrafficLight::Yellow,
        TrafficLight::Yellow => TrafficLight::Red,
    }
}

fn light_duration(light: TrafficLight) -> i32 {
    match light {
        TrafficLight::Red => 30,
        TrafficLight::Yellow => 5,
        TrafficLight::Green => 25,
    }
}
```
