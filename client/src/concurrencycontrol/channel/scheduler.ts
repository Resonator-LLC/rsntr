// A scheduler for a single task, typically one that backs off the more
// often `schedule` is called.
//
// Derived from `org.waveprotocol.wave.model.util.Scheduler` and
// `ImmediateExcecutionScheduler` of Apache Wave, licensed under the Apache
// License, Version 2.0.

/** A simple command that can be invoked. */
export interface SchedulerCommand {
  execute(): void;
}

export interface Scheduler {
  /** Resets the backoff time and the scheduled job. */
  reset(): void;

  /**
   * Schedules a task to be run in the future. A previously scheduled task
   * is cancelled. Returns false if the task was rejected.
   */
  schedule(task: SchedulerCommand): boolean;
}

/**
 * Does no scheduling: runs the task immediately, within `schedule`. For
 * tests only; a real scheduler must back off.
 */
export class ImmediateExecutionScheduler implements Scheduler {
  reset(): void {
    // Do nothing.
  }

  schedule(task: SchedulerCommand): boolean {
    task.execute();
    return true;
  }
}
