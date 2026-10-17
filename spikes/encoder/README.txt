Spike 2 kit: which hardware encoder this PC has, and whether it holds a 1080p30 stream

Before you start
- StreamLoop 2.x and OBS must not be streaming. Their encoder would skew every number.
- The results need about 200 MB of free space next to the kit.

Run it
1. Copy the whole spike2-kit folder to the streaming PC. Any folder works.
2. Double-click run.cmd. If Windows warns about it, choose More info, then Run anyway.
   It runs on its own for about 50 minutes, or about 70 with the Twitch step. Leave the PC
   idle until it finishes: a game or video running beside it skews the numbers.
3. When the window says Finished, zip the results-<PC name>-<date> folder inside spike2-kit
   and send it back.

Twitch step (optional)
The kit sends a 15 minute Twitch bandwidth test, which viewers never see, but only when the PC
has no NVIDIA encoder and a stream key is set. To include it, open a command prompt in the
folder and run:
    set TWITCH_STREAM_KEY=<your key>
    run.cmd
Only do this while 2.x is not live on that channel. The key is never printed or saved. After
the run, check Twitch Inspector for a stable session.

Nothing is installed and the registry is not touched. Deleting the folder removes everything.
