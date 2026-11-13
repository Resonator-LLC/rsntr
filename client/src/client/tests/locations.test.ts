import { describe, expect, test } from "vitest";

import { DocOpBuilder } from "../../model/docop";
import { transformLocation, transformRange } from "../locations";

describe("transformLocation", () => {
  const insert = new DocOpBuilder().retain(3).characters("ab").retain(4).build();
  const remove = new DocOpBuilder().retain(2).deleteCharacters("xyz").retain(2).build();

  test("insertions shift what lies behind them", () => {
    expect(transformLocation(0, insert)).toBe(0);
    expect(transformLocation(2, insert)).toBe(2);
    expect(transformLocation(4, insert)).toBe(6);
    expect(transformLocation(7, insert)).toBe(9);
  });

  test("an insertion at the location follows the bias", () => {
    expect(transformLocation(3, insert, "before")).toBe(3);
    expect(transformLocation(3, insert, "after")).toBe(5);
  });

  test("deletions pull back, and collapse what was inside", () => {
    expect(transformLocation(1, remove)).toBe(1);
    expect(transformLocation(2, remove)).toBe(2);
    expect(transformLocation(3, remove)).toBe(2);
    expect(transformLocation(4, remove)).toBe(2);
    expect(transformLocation(5, remove)).toBe(2);
    expect(transformLocation(7, remove)).toBe(4);
  });

  test("ranges keep insertions at their edges outside", () => {
    const atStart = new DocOpBuilder().retain(2).characters("Q").retain(5).build();
    const atEnd = new DocOpBuilder().retain(5).characters("Q").retain(2).build();
    expect(transformRange({ start: 2, end: 5 }, atStart)).toEqual({ start: 3, end: 6 });
    expect(transformRange({ start: 2, end: 5 }, atEnd)).toEqual({ start: 2, end: 5 });
    expect(transformRange({ start: 2, end: 2 }, atStart)).toEqual({ start: 2, end: 2 });
    expect(transformRange({ start: 2, end: 2 }, atStart, "after")).toEqual({ start: 3, end: 3 });
    expect(transformRange({ start: 3, end: 4 }, remove)).toEqual({ start: 2, end: 2 });
  });
});
