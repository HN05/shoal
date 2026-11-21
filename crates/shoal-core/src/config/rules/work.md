Run `shoal ready` when you need the user before you can go on: your work is ready for review, or you have a decision or answer to ask them for.
Ask your coordinator, or else the user, in a thread: `shoal thread open "<name>"`, then `shoal send <thread> "<question>"`.
Run `shoal thread switch <thread>` before working on a thread, so your changes go to it, and answer what Shoal asks about your changes with `shoal changes claim` or `unclaim`.
Resolve each local review finding's thread with `shoal thread resolve`.
Set your status line with `shoal progress` at each milestone and when you are blocked.
Give separate work its own space.
Open an issue you fix in your own space with `shoal issue open --link`, and link one you take on later with `shoal link issue <number> assigned`.
