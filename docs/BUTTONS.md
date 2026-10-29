# Buttons

Dolphone keeps FlipCTL's roles for the five soft keys everywhere: **Escape** backs out
or cancels, **View** switches view or shows information, **Edit** edits, **Run** does
the main thing. **Power** is never ours: it is the MCU's, and opens the power menu.
**Back** and the **app switcher** never reach an app either: Back leaves the app (on a
call, the call carries on; come back to it with the switcher), and the switcher lists
running apps, a live call among them.

## Dialer

| Screen | D-pad / Ok | Escape | View | Edit | Run | PTT | Touchpad |
|---|---|---|---|---|---|---|---|
| Keypad | move, press a key (hold 0 for +) | clear, then exit | Recents | delete (hold: clear) | Call (empty: redial) | | move |
| Recents | move | back | Contacts | text | call back | | scroll |
| Contacts | move, Left/Right jump a letter | back | Keypad | text | call | | scroll |
| Incoming call | | **Decline** | number details | reply with a text | **Answer** | silence the ringer | |
| Reply picker | choose | back | | | send and decline | | |
| In call | Ok swaps two calls | | keypad | audio route | **End** (ignored for 1.5 s after answering) | mute / unmute | |
| In-call keypad | move, Ok sends the tone | close | close | audio route | End | | move |
| Confirmation | | **Deny** | | | **Allow** | | |
| Service code reply | | close | | reply on the keypad | | | |

Keys are ignored for 400 ms after a call or a confirmation appears unasked, so a
press meant for whatever was on screen cannot answer a call or allow a request.

## Messages

| Screen | D-pad / Ok | Escape | View | Edit | Run |
|---|---|---|---|---|---|
| Threads | move, Ok opens | exit | | new message | open |
| Conversation | Up/Down scroll, Left/Right page, Ok writes | back | options | write | call |
| Options | move | back | back | | select |
| Keyboard (FlipCTL's) | type | cancel | 123 | backspace | done |
| Preview | | edit again | | edit again | **Send** |

## Phone Settings

| | Left / Right | Ok / Run | Edit | Escape |
|---|---|---|---|---|
| ON/OFF rows | toggle | toggle | | up a page |
| `< value >` rows | step | step | | up a page |
| Pages (Call audio, Bluetooth, Blocked, Modem, About) | Right opens | open | | up a page |
| Bluetooth device | | pair / connect / disconnect | forget | |
| Blocked number | | unblock | unblock | |
