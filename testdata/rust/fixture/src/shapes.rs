pub enum Shape {
    Circle(crate::Circle),
    Square(crate::Square),
}

impl crate::Area for Shape {
    fn area(&self) -> f64 {
        match self {
            Shape::Circle(c) => crate::Circle::area(c),
            Shape::Square(s) => crate::Square::area(s),
        }
    }
}

pub fn shape_label(s: &Shape) -> String {
    match s {
        Shape::Circle(_) => "circle".to_string(),
        Shape::Square(_) => "square".to_string(),
    }
}