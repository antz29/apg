export interface Point {
  x: number;
  y: number;
}

export type PointMap = Record<string, Point>;

export enum ShapeKind {
  Circle = "circle",
  Square = "square",
}

export interface Shape {
  kind: ShapeKind;
  area(): number;
}