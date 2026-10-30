# Buttons

Dolphone keeps FlipCTL's roles for the five soft keys everywhere: **Escape** backs out
or cancels, **View** switches view or shows information, **Edit** edits, **Run** does
the main thing. The middle soft key, silkscreened **power**, does reach the app in
front (flipctl forwards it like the other four), but Dolphone leaves its slot empty
on purpose: a press meant for the phone's power is never taken as Answer, End or
Allow. **Back** and the **app switcher** never reach an app: flipctl keeps them. Back
leaves the app (on a call, the call carries on; come back to it with the switcher),
and the switcher lists running apps, a live call among them.

A **hold** is 0.6 s or more by default (the `long_press_ms` setting, 300–3000 ms).

The touchpad is read by the app that has the keyboard, the one in front; an app in
the background ignores it.

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
| Emergency alert | Ok dismisses | dismiss | | | **Dismiss** (**Next** while more wait) | | |
| Confirmation | | **Deny** | | | **Allow** | | |
| Service code reply | | close | | reply on the keypad | | | |

Emergency alerts not yet dismissed wait in turn rather than replacing each other:
the most severe first (National, Extreme, Severe, Amber, Test), alerts of one level
in the order they came, with "1 of 3" in the breadcrumb while more wait. A worse
alert arriving takes the screen at once; the one it covered comes back after it.

Keys are ignored for 400 ms after a call, an alert or a confirmation appears unasked,
so a press meant for whatever was on screen cannot answer a call or allow a request.
A press ignored like this, End in the 1.5 s after answering, and a short press of
Decline when it needs a hold each give the "refused" stutter, so a press that did
nothing does not feel like one that did.

The touchpad works as a keypad once it has learnt its own size from a few strokes;
until then it moves the selection like the D-pad.

### Vibrations

Each kind of news has its own feel, so it can be told in a pocket. Messages uses
the same feel for a text. Everything but a key, a thump, a refused press and an
emergency alert stays still when the ringer is on Silent.

| | |
|---|---|
| a key | a tick |
| a call connects or ends | a thump |
| a text | two short |
| a missed call | three short |
| a second call waiting | two long |
| a call dropped (not ended here) | one long |
| a text not sent | short, short, long |
| an emergency alert | the alert tone's rhythm (long, short, short, twice), even on silent |
| a press ignored on purpose | a quick stutter (three very short) |

## Messages

| Screen | D-pad / Ok | Escape | View | Edit | Run |
|---|---|---|---|---|---|
| Threads | move, Ok opens | exit | | new message | open |
| Conversation | Up/Down scroll, Left/Right page, Ok writes | back | options | write | call |
| Options (Resend failed text, Quick reply, Attach a picture, Call, Block, Delete) | move | back | back | | select |
| Quick reply | choose | back | | edit it first | use (to the preview) |
| Attach a picture | choose, Ok as Run | back | | | **Add** (to the preview), or **Remove** one already added |
| Keyboard (FlipCTL's) | type | cancel | 123 | backspace | done |
| Preview | | edit again | **Attach** (**Pictures** once one is added) | edit again | **Send** |
| Locked (a Messages PIN is set) | Ok types the PIN | exit | | type the PIN | **Unlock**: type the PIN |
| PIN keyboard (opens on 123, the PIN shown as \*) | type | exit Messages | ABC | backspace | **Unlock** (4 to 12 digits) |

With a Messages PIN set (Settings > Privacy), Messages opens on the PIN and shows
nothing of the texts until it is right; it stays open for the time set there, then
asks again. A wrong PIN says so; after five, phoned makes you wait. A number the
Dialer hands over while locked opens once the PIN is in.

**Attach a picture** lists the newest 50 pictures (JPEG, PNG, GIF, WebP) in the
Pictures and Downloads folders (XDG's, from `user-dirs.dirs`, else `~/Pictures` and
`~/Downloads`) and the folders directly in them, with each one's size and date. Links
are not followed, and a file phoned would not send (empty, unreadable, over 8 MB) is
left out. What is added is named above the text on the preview, which is then a
picture message (a text is optional); to take one off, open the pictures again and
**Remove** it. Up to 10, 8 MB in all, go in one message; a group's goes to everyone.
Without pictures the preview sends a text, as before.

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
| Privacy > Messages PIN | Right sets | **Set** / **Change**: the new PIN on the keyboard, then again | **Remove** (asks first) | | up a page |
| Privacy > Messages stay open | step 1, 5, 15 min | step | | | up a page |
| Modem page | | **Set up** when the modem's setup has changes to make, or **Turn on** when ModemManager's debug mode is off; both ask first | | **Network** | up a page |
| Modem > Network (the serving cell, then its neighbours; asks again every 3 s while open; Up and Down scroll) | | **Refresh** | | | back to Modem |

## Desktop app

| Keys | |
|---|---|
| Ctrl+1 … Ctrl+4 | Calls, Messages, Contacts, Settings |
| Ctrl+N | a new message |
| Ctrl+Enter | answer the ringing call |
| Ctrl+Backspace | decline the ringing call, or end the live one |
| Ctrl+M | mute or unmute |
| Tab, Shift+Tab | the next or last control; over a call, a confirmation or an alert, round that card's own buttons only |
| Up, Down | along a list (calls, conversations, contacts, the lists in Settings) |
| Enter | a list row's main thing (call, open, unblock...), or press the control with the focus |
| Space | choose a list row, or press the control with the focus |

The call card opens with the focus on **Answer** (on **Keypad** once the call is
live, away from End); a confirmation on **Deny**; an alert on **Dismiss** (**Next
alert** while more wait, with "1 of 3" beside the time; the most severe comes first).

With a Messages PIN set, the Messages page is a PIN field and **Unlock** (Enter
unlocks too) until it is right; once open, **Lock** beside New closes it again.
Settings > Messages PIN sets or changes it (the new PIN twice), removes it, and
picks how long Messages stays open (1, 5 or 15 minutes).

In a conversation, **Attach** opens the desktop's file chooser (through the XDG
desktop portal) for pictures; what is picked is named above the text, **Remove**
drops it, and **Send** sends the lot as one picture message. Without a portal,
Attach says so.
