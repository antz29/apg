use rustfixture::{Circle, total_area};

#[test]
fn integration_uses_shapes() {
    let shapes = vec![rustfixture::shapes::Shape::Circle(Circle::new(1.0))];
    assert!(total_area(&shapes) > 3.0);
}