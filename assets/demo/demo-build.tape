Output "/home/rayr/Desktop/farhand/assets/demo/demo-build.gif"

Set Shell bash
Set TypingSpeed 8ms
Set FontFamily "JetBrains Mono"
Set FontSize 15
Set Width 1060
Set Height 600
Set Padding 22
Set Framerate 12
Set CursorBlink false
Set Theme {
  "background": "#0d1117",
  "foreground": "#e6edf3",
  "black": "#484f58",
  "red": "#ff7b72",
  "green": "#3fb950",
  "yellow": "#d29922",
  "blue": "#71b7ff",
  "magenta": "#bc8cff",
  "cyan": "#39c5cf",
  "white": "#b1bac4",
  "brightBlack": "#6e7681",
  "brightRed": "#ff7b72",
  "brightGreen": "#56d364",
  "brightYellow": "#e3b341",
  "brightBlue": "#79c0ff",
  "brightMagenta": "#d2a8ff",
  "brightCyan": "#56d4dd",
  "brightWhite": "#f0f6fc"
}

Type "cd /tmp/farhand-demo/app"
Enter
Sleep 400ms

# First run: fresh remote workspace -> full delta sync + build + artifact pull
Type "fh --verbose -- cargo build --release"
Enter
Sleep 3000ms

# Second run: persistent workspace + CAS -> zero bytes transferred
Type "fh --verbose -- cargo build --release"
Enter
Sleep 3000ms
