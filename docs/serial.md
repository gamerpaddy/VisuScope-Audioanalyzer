# Serial port (COM): turntables, relays, an Arduino

VisuScope can send text commands to a controller on a serial port. It only sends; what the text means is up to your controller.

Set it up in *Options > Serial port (COM)*: port, baud rate, line ending, and a list of named commands (name + text to send). Hover the heading for the full help.

- In the text, `\n` `\r` `\t` and `\xNN` stand for control characters; `{angle}` is replaced by the angle of the turntable.
- Send a command with its **Send** button, from the command palette (Ctrl+K), from a key / MIDI / controller binding (*Keys, MIDI and controllers*, action `serial:<name>`), or from a test sequence with the line `serial <text>`.
- *Speaker design > Polar* has a **Turntable** switch: it sends the command (default `G{angle}`) before each angle, waits the settle time and measures. **Measure all** runs every angle up to the maximum by itself.
- Most boards restart when the port is opened, so the first command waits (default 2 s, "boot ms").

A minimal Arduino sketch for a relay and a stepper table:

```cpp
void setup() { Serial.begin(115200); pinMode(7, OUTPUT); }
void loop() {
  if (!Serial.available()) return;
  String s = Serial.readStringUntil('\n');
  if (s == "R1") digitalWrite(7, HIGH);        // command "relay on"  = R1
  else if (s == "R0") digitalWrite(7, LOW);    // command "relay off" = R0
  else if (s[0] == 'G') turnTo(s.substring(1).toFloat());  // G45 = turn the table to 45 degrees
}
```
