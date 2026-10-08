## What it means

A process id (pid) names a process only while that process runs. Once it
exits, any other process can get the number, so icm never signals a pid it
has not checked. When icm starts a process (the desktop app, the iOS
Simulator's app and its log collector, the emulator, the web session, an iOS
device's console) it reads the process's start time right away and keeps it
beside the pid in the session record. Before a later command signals the
pid, or decides about a device because "its process still runs", it reads
the pid again and compares.

This warning means that the first read failed: the OS would not describe
the process, or the process had already exited. The record then holds an
explicit `identity.unavailable` with the reason, in place of a start time.
That is not the same as a record without an identity, which an icm older
than process identities wrote, and icm does not judge it by the pid as it
still does for those: the pid is not taken for the process, and nothing is
signalled.

The same warning comes from `stop`, and from the next `run` of the desktop,
the iOS Simulator and ios-device (which replaces a running session), when a
process runs under such a pid, or under a pid whose process the OS will not
describe now: icm left it alone and says so, since "nothing was running"
would be false. The summary of `stop` names the process it left running
(`desktop pid 4242 left running (cannot be verified)`), and `logs desktop`
gives it as the reason it could not read the app's environment when the app
inherited a secret.

The web session host is stricter: one that cannot read its own identity does
not start (`web.host_identity`), since nothing could stop it later.

## Fix

Check what runs under the pid in the detail (`ps -p <pid> -o
pid,lstart,command`), and stop it yourself if it is the process icm started:
the pid may belong to something else by now, which is why icm will not
signal it. A process that ends by itself needs nothing. Once nothing runs
under the pid, `icm stop <platform>` removes the record.
