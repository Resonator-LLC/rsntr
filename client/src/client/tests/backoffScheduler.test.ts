import { expect, test } from "vitest";

import { BackoffScheduler } from "../backoffScheduler";
import { SimLoop } from "../testing/simulation";

test("the first task runs at once, later ones after growing delays, until reset", () => {
  const loop = new SimLoop(1);
  const scheduler = new BackoffScheduler({ timers: loop, random: () => 0.5, initialMs: 100, maxMs: 350 });
  const ran: number[] = [];
  const task = { execute: () => ran.push(loop.now()) };

  const expectDelay = (delay: number): void => {
    const start = loop.now();
    expect(scheduler.schedule(task)).toBe(true);
    loop.runUntilIdle();
    expect(ran[ran.length - 1]! - start).toBe(delay);
  };
  expectDelay(0);
  expectDelay(100);
  expectDelay(200);
  expectDelay(350);
  expectDelay(350);
  scheduler.reset();
  expectDelay(0);
  expectDelay(100);
});

test("jitter spreads the delay; cancel drops pending tasks", () => {
  const loop = new SimLoop(1);
  let random = 0;
  const scheduler = new BackoffScheduler({ timers: loop, random: () => random, initialMs: 100, jitter: 0.3 });
  const ran: number[] = [];
  const task = { execute: () => ran.push(loop.now()) };
  scheduler.schedule(task);
  loop.runUntilIdle();
  random = 0;
  scheduler.schedule(task);
  loop.runUntilIdle();
  expect(ran[1]! - ran[0]!).toBe(70);
  random = 0.999999;
  const start = loop.now();
  scheduler.schedule(task);
  loop.runUntilIdle();
  expect(ran[2]! - start).toBe(260);

  scheduler.schedule(task);
  scheduler.cancel();
  loop.runUntilIdle();
  expect(ran.length).toBe(3);
  expect(scheduler.schedule(task)).toBe(false);
});
