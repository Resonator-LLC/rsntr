import { describe, expect, test } from "vitest";

import { formatFullDateTime, formatMediumDate, formatPastDate, formatTime } from "./dates";

// Local times, so that the tests hold in any time zone.
const at = (y: number, mo: number, d: number, h = 0, mi = 0): Date => new Date(y, mo - 1, d, h, mi);

describe("formatPastDate (DateUtils)", () => {
  const now = at(2026, 11, 11, 15, 30);

  test("times", () => {
    expect(formatTime(at(2026, 1, 1, 0, 5))).toBe("12:05 am");
    expect(formatTime(at(2026, 1, 1, 9, 0))).toBe("9:00 am");
    expect(formatTime(at(2026, 1, 1, 12, 0))).toBe("12:00 pm");
    expect(formatTime(at(2026, 1, 1, 23, 59))).toBe("11:59 pm");
  });

  test("today: the time", () => {
    expect(formatPastDate(at(2026, 11, 11, 15, 29), now)).toBe("3:29 pm");
    expect(formatPastDate(at(2026, 11, 11, 0, 1), now)).toBe("12:01 am");
  });

  test("within six hours, across midnight: the time", () => {
    const early = at(2026, 11, 11, 2, 0);
    expect(formatPastDate(at(2026, 11, 10, 22, 15), early)).toBe("10:15 pm");
    // More than six hours ago and yesterday: day and time.
    expect(formatPastDate(at(2026, 11, 10, 19, 59), early)).toBe("Nov 10 7:59 pm");
  });

  test("within thirty days: day and time", () => {
    expect(formatPastDate(at(2026, 11, 10, 9, 5), now)).toBe("Nov 10 9:05 am");
    expect(formatPastDate(at(2026, 10, 13, 16, 0), now)).toBe("Oct 13 4:00 pm");
  });

  test("this year: the day", () => {
    expect(formatPastDate(at(2026, 10, 1, 16, 0), now)).toBe("Oct 1");
    expect(formatPastDate(at(2026, 1, 31, 8, 0), now)).toBe("Jan 31");
  });

  test("another year: the date", () => {
    expect(formatPastDate(at(2025, 12, 24, 8, 0), now)).toBe("Dec 24, 2025");
    expect(formatMediumDate(at(2009, 5, 28))).toBe("May 28, 2009");
    // Within thirty days wins over the year, as in the original.
    expect(formatPastDate(at(2025, 12, 31, 23, 0), at(2026, 1, 2, 12, 0))).toBe("Dec 31 11:00 pm");
  });

  test("the future is formatted anyway", () => {
    expect(formatPastDate(at(2026, 11, 11, 18, 0), now)).toBe("6:00 pm");
    expect(formatPastDate(at(2026, 11, 12, 18, 0), now)).toBe("6:00 pm");
  });

  test("numbers and nonsense", () => {
    expect(formatPastDate(at(2026, 11, 11, 15, 29).getTime(), now.getTime())).toBe("3:29 pm");
    expect(formatPastDate(Number.NaN, now)).toBe("");
    expect(formatFullDateTime(at(2026, 3, 4, 13, 7))).toBe("Mar 4, 2026 1:07 pm");
    expect(formatFullDateTime(Number.NaN)).toBe("");
  });
});
