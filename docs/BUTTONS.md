# Buttons

Dolphone keeps FlipCTL's roles for the five soft keys everywhere: **Escape** backs out
or cancels, **View** switches view or shows information, **Edit** edits, **Run** does
the main thing. **Power** is never ours: it is the MCU's, and opens the power menu.
**Back** and the **app switcher** never reach an app either: Back leaves the app (on a
call, the call carries on; come back to it with the switcher), and the switcher lists
running apps, a live call among them.

A **hold** is 0.6 s or more by default (the `long_press_ms` setting, 300–1500 ms).

## Dialer

| Screen | D-pad / Ok | Escape | View | Edit | Run | PTT | Touchpad |
|---|---|---|---|---|---|---|---|
| Keypad | move, press a key; hold 0 for +; on an empty keypad hold 1 for voicemail, hold 2–9 for speed dial | clear, then exit | Recents | delete (hold: clear) | Call (empty: redial; a name matched by its letters, "567" for Jordan: call them) | hold: Ring → Vibrate → Silent | tap a key, swipe to move |
| Keypad, SIM locked | type the PIN (shown as \*) | clear | Recents | delete | **Unlock** (an emergency number still dials) | | tap a key |
| Recents | move | back | Contacts | text | call back | | swipe |
| Contacts | move, Left/Right jump a letter; Ok calls, hold Ok to give the contact a speed-dial key | back | Keypad | text | call | | swipe |
| Speed dial picker | choose 2–9 | back | | clear that key | set | | |
| Incoming call | | **Decline** (hold, with "Hold to decline" on) | number details | reply with a text | **Answer** | silence the ringer | |
| Reply picker | choose | back | | | send and decline | | |
| In call | Ok swaps two calls; hold Ok merges them | | keypad | audio route | **End** (ignored for 1.5 s after answering) | mute / unmute | |
| In-call keypad | move, Ok sends the tone | close | close | audio route | End | | tap a key |
| Emergency alert | Ok dismisses | dismiss | | | **Dismiss** | | |
| Confirmation | | **Deny** | | | **Allow** | | |
| Service code reply | | close | | reply on the keypad | | | |

Keys are ignored for 400 ms after a call, an alert or a confirmation appears unasked,
so a press meant for whatever was on screen cannot answer a call or allow a request.

The touchpad works as a keypad once it has learnt its own size from a few strokes;
until then it moves the selection like the D-pad.

### Vibrations

Each kind of news has its own feel, so it can be told in a pocket:

| | |
|---|---|
| a key | a tick |
| a call connects or ends | a thump |
| a text | two short |
| a missed call | three short |
| a second call waiting | two long |
| a call dropped (not ended here) | one long |
| a text not sent | short, short, long |
| an emergency alert | three long, even on silent |

## Messages

| Screen | D-pad / Ok | Escape | View | Edit | Run |
|---|---|---|---|---|---|
| Threads | move, Ok opens | exit | | new message | open |
| Conversation | Up/Down scroll, Left/Right page, Ok writes | back | options | write | call |
| Options (Resend failed text, Quick reply, Call, Block, Delete) | move | back | back | | select |
| Quick reply | choose | back | | edit it first | use (to the preview) |
| Keyboard (FlipCTL's) | type | cancel | 123 | backspace | done |
| Preview | | edit again | | edit again | **Send** |

A picture message shows its text with `[Picture]` (or `[Audio]`, `[Contact]`) below:
the panel is too small for the picture, which the desktop app shows. A group
conversation is one thread, named after everyone on it; a reply goes to all of them.

## Phone Settings

| | Left / Right | Ok / Run | Edit | View | Escape |
|---|---|---|---|---|---|
| ON/OFF rows | toggle | toggle | | | up a page |
| `< value >` rows | step | step | | | up a page |
| Pages (Call audio, Bluetooth, Laptops, Calls, Calls over the internet, Texts, Emergency alerts, Privacy, Modem, About) | Right opens | open | | | up a page |
| Typed rows (voicemail number, SIP account, password, proxy) | | open the keyboard | open the keyboard | | up a page |
| Bluetooth device | | pair / connect / disconnect | forget | copy contacts (a paired phone) | |
| Blocked number | | unblock | unblock | | |
| A program allowed to text | | revoke | revoke | | |
| Modem page | | **Set up** when the modem's setup has changes to make, or **Turn on** when ModemManager's debug mode is off; both ask first | | | up a page |

## Desktop app

| Keys | |
|---|---|
| Ctrl+1 … Ctrl+4 | Calls, Messages, Contacts, Settings |
| Ctrl+N | a new message |
| Ctrl+Enter | answer the ringing call |
| Ctrl+Backspace | decline the ringing call, or end the live one |
| Ctrl+M | mute or unmute |
