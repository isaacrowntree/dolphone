# Rules on incoming texts

A rule runs a command when a matching text arrives: a text that says STATUS gets
the uplink's state back, a text with a code reboots a router, a one-time code is
handed to a script. Rules live in `~/.config/dolphone/rules.json`, a list:

```json
[
  { "from": "+61491570156", "contains": "STATUS", "secret": "4821",
    "run": ["/usr/local/bin/wan-status"], "reply": true },
  { "from": "*", "contains": "Your code is", "run": ["/home/me/bin/otp-to-clipboard"] }
]
```

| Field | | |
|---|---|---|
| `from` | required | the sender, in any form (`0491 570 156`, `+61491570156`), or `*` for anyone |
| `contains` | optional | text the message must contain, ignoring case |
| `secret` | optional | text it must also contain, exactly: a shared password for rules that do something |
| `run` | required | the program and its arguments; run directly, never through a shell |
| `reply` | optional | `true` to text back what the program prints |

A field phoned does not know makes the whole file invalid (it says so in its log)
rather than being ignored: a misspelled `"contains"` would otherwise leave a rule
that fires on every text from that sender. phoned reads the file again for every
text, so changes count at once.

## What the program gets

The text as JSON on its standard input:

```json
{"from": "+61491570156", "body": "STATUS 4821", "at": 1793228520}
```

`at` is when it was sent, in Unix seconds. The program has 30 seconds; after that it
is stopped. Every rule that matches runs, each on its own, at the same time.

With `"reply": true`, what the program prints (up to 600 characters) is texted back
to the sender, but only to a sender the rule names: a rule for `*` never replies,
so a rule cannot be made to text anyone who writes to it. A program that exits with
an error sends no reply, and what it printed to its standard error goes to phoned's
log (`journalctl --user -u phoned`), for you to see why.

## What never runs a rule

- Texts that arrive over SIP: a SIP sender is not authenticated the way a mobile
  network's is, and a reply goes out as a text on your account.
- Texts from blocked numbers, and flash texts, which are shown and not kept.

## An example: STATUS

```sh
#!/bin/sh
# /usr/local/bin/wan-status: say whether the uplink is up.
ip -brief link show eth0 | awk '{print "eth0 is " tolower($2)}'
```

With the first rule above, texting `STATUS 4821` from +61491570156 gets `eth0 is up`
back. For texting when the uplink goes down without being asked, see the
`uplink_alert_to` setting instead.
