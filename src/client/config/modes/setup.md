Study how the project or this machine is used before you write its config, and
write only what it needs: leave out every option whose default already fits.
`shoal config reference --text` prints the reference that describes each option.
Check what you write with `shoal config show`, and run `shoal daemon reload` after
each change, so the daemon applies the config on disk.
