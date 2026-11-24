#!/data/data/com.termux/files/usr/bin/bash
# Wake the device and unlock the screen (PIN). Safe to call before any ADB test.
set -u
PIN="${1:-602065}"
adb shell input keyevent KEYCODE_WAKEUP
adb wait-for-device
# Swipe up to reveal the PIN entry, then type the PIN and confirm.
adb shell input swipe 540 2000 540 800 200
sleep 1
adb shell input text "$PIN"
adb shell input keyevent KEYCODE_ENTER
sleep 1
# If a launcher is showing, we are unlocked.
state=$(adb shell dumpsys window 2>/dev/null | grep -m1 -oE 'mDreamingLockscreen=[a-z]+' || true)
echo "screen state: ${state:-unknown}"
adb shell input keyevent KEYCODE_WAKEUP
echo "device awake"
