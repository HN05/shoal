Study how the project or this machine is used before you write its config, and
write only what it needs: leave out every option whose default already fits.
`shoal config reference --text` prints the reference that describes each option.
Check what you write with `shoal config show`. A repository's config applies as
soon as it is saved; after a change to the global config, run `shoal daemon reload`
so the daemon applies it.
