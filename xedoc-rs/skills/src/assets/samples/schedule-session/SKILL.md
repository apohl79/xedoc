---
name: schedule-session
description: Create a macOS LaunchAgent that periodically submits a user-defined recurring task either to the Xedoc session invoking this skill or to a fresh session started for each run.
---

# Schedule Session

1. Ask the user for a schedule, suggesting:
   - Every 24 hours at 09:00 (`daily-9`)
   - Every 12 hours at 00:00 and 12:00 (`twice-daily`)
   - Every 6 hours (`every-6h`, at 00:00, 06:00, 12:00, 18:00)
   - Every 2 hours (`every-2h`, on even hours)
   - Every hour (`hourly`, on the hour)
   - Custom (`custom`): ask for local times and optional days, then encode the
     exact `StartCalendarInterval` entries as JSON. Do not guess a custom
     schedule.
2. Ask for the exact recurring task to submit on each run. Clarify whether the
   task needs unattended authorization for consequential actions; installing
   a schedule is not approval for actions the task itself requires.
3. Ask which session each run should use. Do not assume either option:
   - Resume this session (`resume`): each run continues this conversation and
     sees its history; runs steer this session if it is busy.
   - Fresh session per run (`fresh`): each run starts a new session in the
     current working directory with no prior history, so the task text must be
     self-contained.
   Ask steps 1–3 together in one question set when the user-input tool allows.
4. For `resume`, the installer obtains the current session ID from
   `XEDOC_THREAD_ID` in the invoking agent's environment, not from another job,
   a copied example, or the latest session in the directory, and fails rather
   than scheduling the wrong session when it is unavailable. Use the current
   working directory. Do not store secrets in the task.
5. For a daily or weekday job, use `--catch-up daily` unless the user
   explicitly wants missed runs skipped. This permits only one local daily
   time, optionally filtered by weekday; do not apply it to hourly,
   multiple-times-per-day, or Day/Month schedules. Preserve an existing job's
   schedule, task text, and session when only catch-up is requested; do not ask
   again or create a duplicate job.
6. Install the schedule only through a supported Xedoc scheduling mechanism.
   Do not create a LaunchAgent or invoke an installer unless its implementation
   is present and verified for this Xedoc version. If no supported mechanism
   is available, explain the blocker and stop rather than guessing at the
   control socket, RPC protocol, session identity, or unattended permissions.
7. Report the label, local schedule, catch-up policy, session mode, task
   summary, and paths returned by the installer. Do not manually trigger the
   newly created job unless requested. Enabling daily catch-up may itself
   submit today's due job when loaded; state this before installation. In
   `resume` mode the job starts an idle session turn or steers an active turn;
   in `fresh` mode it starts a new session and turn. Both use the app-server
   control socket; the job does not enable Matrix or add approvals not
   requested by the user.

## Existing scheduled jobs

Do not modify, reload, or remove an existing LaunchAgent unless the user
explicitly requests that exact operation and a supported Xedoc scheduling
mechanism documents its state and lifecycle behavior. Never infer task
submission or completion from a log or briefing file alone.
