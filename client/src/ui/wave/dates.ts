// How the wave panel shows the time of a blip.
//
// A port of `DateUtils.formatPastDate` of the Apache Wave web client
// (`org.waveprotocol.wave.client.common.util.DateUtils`, Apache License
// 2.0), with `Intl` in place of GWT's `DateTimeFormat`: a time today or in
// the last six hours shows as a time, one in the last thirty days as month,
// day and time, one of this year as month and day, anything older as a
// full date.

const HOUR_MS = 60 * 60 * 1000;
const DAY_MS = 24 * HOUR_MS;

export interface DateFormatOptions {
  /** BCP 47 locale; default: the browser's. */
  readonly locale?: string;
  /** IANA time zone; default: the browser's. */
  readonly timeZone?: string;
}

interface Formats {
  readonly time: Intl.DateTimeFormat;
  readonly monthDay: Intl.DateTimeFormat;
  readonly medium: Intl.DateTimeFormat;
  readonly full: Intl.DateTimeFormat;
  readonly parts: Intl.DateTimeFormat;
}

const cache = new Map<string, Formats>();

function formats(options: DateFormatOptions): Formats {
  const key = `${options.locale ?? ""}|${options.timeZone ?? ""}`;
  let entry = cache.get(key);
  if (entry === undefined) {
    const zone = options.timeZone !== undefined ? { timeZone: options.timeZone } : {};
    const locale = options.locale;
    entry = {
      // `getShortTimeFormat`
      time: new Intl.DateTimeFormat(locale, { hour: "numeric", minute: "2-digit", ...zone }),
      // `PredefinedFormat.MONTH_ABBR_DAY`
      monthDay: new Intl.DateTimeFormat(locale, { month: "short", day: "numeric", ...zone }),
      // `getMediumDateFormat`
      medium: new Intl.DateTimeFormat(locale, { dateStyle: "medium", ...zone }),
      full: new Intl.DateTimeFormat(locale, { dateStyle: "full", timeStyle: "short", ...zone }),
      parts: new Intl.DateTimeFormat("en-US", {
        year: "numeric",
        month: "numeric",
        day: "numeric",
        ...zone,
      }),
    };
    cache.set(key, entry);
  }
  return entry;
}

function dayOf(format: Intl.DateTimeFormat, time: number): { year: string; day: string } {
  let year = "";
  let month = "";
  let day = "";
  for (const part of format.formatToParts(time)) {
    if (part.type === "year") {
      year = part.value;
    } else if (part.type === "month") {
      month = part.value;
    } else if (part.type === "day") {
      day = part.value;
    }
  }
  return { year, day: `${year}-${month}-${day}` };
}

/** `DateUtils.formatTime`: the short time, lower-cased ("3:45 pm"). */
export function formatTime(time: number, options: DateFormatOptions = {}): string {
  return formats(options).time.format(time).toLowerCase();
}

/** `DateUtils.formatPastDate`. */
export function formatPastDate(
  time: number,
  now: number = Date.now(),
  options: DateFormatOptions = {},
): string {
  const f = formats(options);
  const then = dayOf(f.parts, time);
  const today = dayOf(f.parts, now);
  if (now - time < 6 * HOUR_MS || then.day === today.day) {
    return formatTime(time, options);
  }
  if (now - time < 30 * DAY_MS) {
    return `${f.monthDay.format(time)} ${formatTime(time, options)}`;
  }
  if (then.year === today.year) {
    return f.monthDay.format(time);
  }
  return f.medium.format(time);
}

/** The full date and time, for tooltips and screen readers. */
export function formatFullDate(time: number, options: DateFormatOptions = {}): string {
  return formats(options).full.format(time);
}
