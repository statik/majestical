import { expect, test } from "vitest";
import { duration, fileSize, isoDay, timecode } from "./format";

test("bytes below a kilobyte are counted exactly, in bytes", () => {
  expect(fileSize(0)).toBe("0 B");
  expect(fileSize(1023)).toBe("1023 B");
});

test("every larger unit is one decimal place", () => {
  expect(fileSize(1024)).toBe("1.0 KB");
  expect(fileSize(4_400_000_000)).toBe("4.1 GB");
});

test("the largest unit holds rather than inventing one past it", () => {
  // 4 PB — the loop stops at the last unit it knows instead of walking off
  // the end of the table.
  expect(fileSize(4 * 1024 ** 5)).toBe("4.0 PB");
});

test("a day is the ISO day the CLI prints", () => {
  expect(isoDay(1_700_000_000_000)).toBe("2023-11-14");
});

test("a timecode is the spelling `maj search` prints", () => {
  expect(timecode(0)).toBe("@0m00s");
  expect(timecode(90_000)).toBe("@1m30s");
});

test("a duration is minutes and seconds, and grows an hours field", () => {
  expect(duration(0)).toBe("00:00");
  expect(duration(372_000)).toBe("06:12");
  expect(duration(3_600_000)).toBe("1:00:00");
  // Truncated, not rounded: a run 59.9 seconds in has not reached a minute.
  expect(duration(59_900)).toBe("00:59");
});
