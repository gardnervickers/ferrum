# "Queue is not empty after probing" (2026-11-17)

Seen once in the simulator: a job's auto stock walk stopped with LinuxCNC's
"Queue is not empty after probing" and ran clean on the next try.

## Cause

A timing race inside LinuxCNC 2.9.10, between task and the motion controller.
millgui, the job program and the subroutines are not involved.

1. Task sends a G38 move to motion and waits for the command echo
   (`usrmotWriteEmcmotCommand`).
2. Motion's command handler (`emcmotCommandHandler`, `command.c`) queues the
   move and publishes the echo. It does not clear the in-position flag; the
   flag and the queue depth are only updated by the controller function
   (`emcmotController`, `control.c`), which runs right after the handler in
   the servo thread.
3. If task reads motion status in the gap between the two functions, it sees
   the echo together with the old "in position, queue empty" state and takes
   the probe move as finished (`emcTrajUpdate`: `inpos && queue == 0` is
   `RCS_DONE`).
4. Task lets the interpreter read on. One cycle later the status is fresh,
   the queue holds the probe move, and `Interp::read_inputs` raises
   "Queue is not empty after probing".

The gap is normally well under a microsecond, because `mymill.hal` runs
`motion-command-handler` and `motion-controller` back to back. It only opens
when the servo thread is descheduled exactly there, which a non-realtime
cloud simulator under load does now and then and a PREEMPT_RT Pi almost
never does.

## How it was confirmed (simulator only)

- 2,400 `G38.3` moves in AUTO on stock 2.9.10, with and without 8 busy
  loops on 4 cores: no error.
- A 0.4 ms busy wait added at the end of the command handler for
  `EMCMOT_PROBE` (widening the gap): the same program fails with
  "Queue is not empty after probing".
- With that wait still in place, one more line in the `EMCMOT_PROBE` case
  makes 800 moves run clean again:

```c
	    } else {
		emcmotStatus->probing = 1;
		SET_MOTION_INPOS_FLAG(0);   /* the queued move is not done */
```

The simulator is back on unpatched 2.9.10.

## What it means on the mill

- When it happens the program stops with the error before the next block
  runs. Nothing is written: the auto stock result is only applied by
  `probe_auto_stock_apply`, after the whole walk.
- It can also happen from the PROBE screen (MDI), not only in a job.
- Starting the job (or the probe) again is the remedy.
- A fix belongs in LinuxCNC (the line above, or the same in upstream). Not
  applied to the Pi; that needs a rebuilt `motmod` and is Gardner's call.
