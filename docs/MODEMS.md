# Choosing a modem

Dolphone works with any modem ModemManager supports, but what the phone can do
depends on the modem, and then on the SIM in it. Nothing here has been tried on real
hardware yet; every claim below comes from the source linked beside it.

## Three different limits

These get confused, and they fail in different ways:

| | What it means | What Dolphone does |
|---|---|---|
| **A data-only modem** | The modem has no voice support at all (ModemManager shows no Voice interface). Most M.2 5G modules are like this, the RM520N-GL included. Some still send and receive texts. | It knows straight away. The Dialer says "texts only, no calls"; dialling says "this modem cannot make calls"; 000 says to call from another phone; the modem's setup leaves call audio and VoLTE alone. Texts, picture messages and calls over SIP still work. |
| **A data-only SIM** (or plan) | The modem could call, but your plan has no calls or texts. | It cannot tell until the network refuses: a call fails when dialled, a text fails when sent. Check the plan, not the modem. |
| **No VoLTE on your network** | The modem and the plan can call, but the carrier will not carry this modem's VoLTE calls. In Australia, where 3G is gone, that means no mobile calls at all, and the ACMA rules have carriers block a device that can call but cannot reach 000 over VoLTE ([ACMA](https://www.acma.gov.au/ensuring-mobiles-can-reach-000-after-3g-shutdown)). | Calls fail to connect. Ask the carrier before buying. |

**Calls over the internet (SIP)** need none of this: a SIP account works over any
data link, a data-only modem, a data-only SIM or Wi-Fi. SIP cannot reach emergency
services; only a modem that can call does.

![The Dialer on a data-only modem: Fakecom AU - texts only, no calls](screens/dialer-keypad-texts-only.png)

## Which modem for what

| You want | Buy | Why, and the catch |
|---|---|---|
| **Texts and data, no mobile calls** | Quectel **RM520N-GL** (5G, M.2) | Quectel's staff say it is "data-only" and will not get voice ([forum](https://forums.quectel.com/t/quectel-rm520n-gl-and-volte/32316), [forum](https://forums.quectel.com/t/voice-call-with-the-rm520n-gl/34404)). A user reports texts work on it (same thread; not confirmed by Quectel). Add a SIP account for calls. |
| **Mobile calls, in an M.2 slot** | SIMCom **SIM7600G-H-M2** (4G, M.2 Key B) | SIMCom's specification lists VoLTE and USB audio; its call audio is raw PCM on a serial port, which Dolphone bridges to PipeWire (the uConsole profile does this) ([SIMCom USB audio note](https://files.waveshare.com/upload/8/8e/SIM7100_SIM7500_SIM7600_Series_USB_AUDIO_Application_Note_V1.03.pdf), [Waveshare](https://waveshare.com/wiki/SIM7600G-H-M.2)). Nobody has published a working Australian VoLTE call on it, and a ModemManager bug report says its in-call audio sometimes fails to set up ([issue 797](https://gitlab.freedesktop.org/mobile-broadband/ModemManager/-/issues/797)). |
| **Mobile calls with a USB sound card** | Quectel **EG25-G** (4G, mPCIe) | The PinePhone's modem. A user reports its USB sound card (UAC) works after a firmware update ([forum](https://forums.quectel.com/t/eg25-and-uac-for-audio/31663)). VoLTE is reported working on ALDI Mobile and Amaysim, but not on Telstra directly, and Quectel says it is not Telstra-certified ([forum](https://forums.quectel.com/t/eg25-g-firmware-request-volte-not-registering-on-australian-carriers/56633), [Pine64 forum](https://forum.pine64.org/showthread.php?tid=12560)). It is mPCIe, so an M.2 slot needs an adapter, which nobody has tried on a Flipper One. |
| **5G and mobile calls** | Nothing confirmed yet | No M.2 5G module was found that is documented to do calls with its audio over USB. The RM520N-EU has voice by Quectel's account, but only in its region ([forum](https://forums.quectel.com/t/which-5g-m-2-modules-support-voice-calls/47294)); the RM500U is said to do VoLTE (same thread), but has PCM pins rather than USB audio ([spec](https://mm.digikey.com/Volume0/opasdata/d220001/medias/docus/7133/Quectel_RM500U_Series_5G_Module_Specification_V1.6.pdf)). |

**Not for calls**, by their makers' or certifiers' accounts: Quectel EM05-G
([forum](https://forums.quectel.com/t/can-the-em05-g-support-volte-calls/53006)),
Sierra EM9191 ([forum](https://forum.sierrawireless.com/t/volte-support-on-em-series/22377)),
Fibocom FM350-GL ([T-Mobile](https://www.t-mobile.com/business/solutions/iot/device-certification/fibocom-fm350-gl)),
Telit FN990 ([NTT](https://www.ntt.com/business/iot/product/m263.html)), SIMCom
SIM8200EA-M2 ([Waveshare](https://www.waveshare.com/wiki/SIM8200EA-M2_5G_HAT)). The
RM500Q and RM502Q register VoLTE for some but calls fail
([forum](https://forums.quectel.com/t/voice-over-usb-volte-rm502q-gl/27890)).

Whether the Flipper One's M.2 slot carries a modem's PCM pins is not published, so
this page assumes it does not: a modem's call audio has to come over USB, as a sound
card or as a serial PCM stream.

If you try one, run [the hardware test](HARDWARE.md) and say what passed: it is how a
row here goes from "reported" to "tested".
