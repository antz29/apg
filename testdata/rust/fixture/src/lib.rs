//! Fixture crate for the apg rust frontend. Exercises the exact-fidelity paths:
//! nested modules, enums with payloads, traits (default + required methods),
//! inherent and trait impls on multiple types (the FQN-collision case), foreign
//! trait impls, generics, macros, std calls, and a test module.

pub mod shapes;

mod inner {
    pub struct Deep {
        pub id: u32,
    }

    impl Deep {
        pub fn new(id: u32) -> Deep {
            Deep { id }
        }
    }
}

pub struct Circle {
    pub radius: f64,
}

#[derive(serde::Serialize)]
pub struct Square {
    pub side: f64,
}

pub enum Color {
    Red,
    Green,
    Blue,
    Rgb(u8, u8, u8),
}

pub trait Area {
    fn area(&self) -> f64;
    fn scale(&self, factor: f64) -> f64 {
        self.area() * factor
    }
}

pub trait Label {
    fn label(&self) -> String;
}

impl Area for Circle {
    fn area(&self) -> f64 {
        std::f64::consts::PI * self.radius * self.radius
    }
}

impl Area for Square {
    fn area(&self) -> f64 {
        self.side * self.side
    }
}

impl Label for Circle {
    fn label(&self) -> String {
        format!("circle(r={})", self.radius)
    }
}

impl Circle {
    pub fn new(radius: f64) -> Circle {
        Circle { radius }
    }

    pub fn diameter(&self) -> f64 {
        2.0 * self.radius
    }
}

pub struct Cache<T> {
    items: Vec<T>,
}

impl<T> Cache<T> {
    pub fn push(&mut self, item: T) {
        self.items.push(item);
    }
    pub fn len(&self) -> usize {
        self.items.len()
    }
}

macro_rules! twice {
    ($e:expr) => {
        $e * 2
    };
}

pub use shapes::Shape;

pub fn total_area(shapes: &[Shape]) -> f64 {
    shapes.iter().map(|s| s.area()).sum::<f64>()
}

pub fn describe(circle: &Circle) -> String {
    let d = circle.diameter();
    let l = circle.label();
    format!("{l} d={d}")
}

pub fn read_file(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

pub fn use_deps(square: &Square) -> String {
    let _ = serde_json::to_string(square).ok();
    let v: Vec<Square> = Vec::new();
    let mut it = v.iter();
    it.next();
    let _ = it;
    String::from("ok")
}

pub fn with_macro(n: i32) -> i32 {
    twice!(n) + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn area_works() {
        let c = Circle::new(2.0);
        assert!(c.area() > 12.0);
        assert!(c.scale(3.0) > 37.0);
    }
}