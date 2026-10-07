## How icm detects it

The Android tools (sdkmanager, avdmanager, apksigner, keytool, jarsigner)
need JDK 17 or newer. icm reads each candidate's version from its `release`
file (or `java -version`) instead of trusting it: `/usr/libexec/java_home -v
17+` can return a Java 8 applet plugin, and `/usr/bin/java` may be Java 8.

Search order: host.toml `java_home`, `$JAVA_HOME`, `java_home -v 17+`,
Homebrew `openjdk@21`/`openjdk@17`/`openjdk`,
`/Library/Java/JavaVirtualMachines/*`, Android Studio's JBR, `/usr/lib/jvm/*`.

Once found, every Android child gets `JAVA_HOME=<jdk>` and `<jdk>/bin` first
on `PATH` (`icm print env android` prints the same).

## Fix

```sh
brew install openjdk@21
# or point icm at an existing JDK in ~/.config/icm/host.toml:
# java_home = "/path/to/jdk/Contents/Home"
```
