import { Point, PointMap, Shape, ShapeKind } from "./shapes";

export class Circle implements Shape {
  kind: ShapeKind = ShapeKind.Circle;
  constructor(private readonly center: Point, private readonly radius: number) {}

  area(): number {
    return Math.PI * this.radius * this.radius;
  }

  get diameter(): number {
    return this.radius * 2;
  }

  set diameter(v: number) {
    this.radius = v / 2;
  }

  static fromCenter(center: Point): Circle {
    return new Circle(center, 1);
  }
}

export function totalArea(shapes: Shape[]): number {
  return shapes.reduce((sum, s) => sum + s.area(), 0);
}

export const scale = (p: Point, f: number): Point => ({ x: p.x * f, y: p.y * f });

export namespace geometry {
  export function perimeter(s: Shape): number {
    return s.area();
  }
  export const registry: PointMap = {};
}