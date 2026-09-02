import { Circle, geometry, scale, totalArea } from "./geometry";
import { ShapeKind } from "./shapes";

export function main(): number {
  const circle = Circle.fromCenter({ x: 0, y: 0 });
  circle.diameter = 10;
  const area = totalArea([circle]);
  const scaled = scale({ x: 1, y: 2 }, area);
  geometry.perimeter(circle);
  console.log("main", ShapeKind.Circle, scaled);
  return area;
}

export function overloaded(a: string): string;
export function overloaded(a: number): number;
export function overloaded(a: string | number): string | number {
  return a;
}