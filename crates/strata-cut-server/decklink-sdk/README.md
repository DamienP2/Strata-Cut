# Blackmagic DeckLink SDK headers

Interface files of the Blackmagic DeckLink SDK 12.0 (Linux), used to build the
DeckLink shim (`../decklink/shim.cpp`, see `../build.rs`):

- C++ headers and `LinuxCOM.h`;
- `DeckLinkAPIDispatch.cpp`, which loads `libDeckLinkAPI.so` with `dlopen` at
  run time.

They are copied unmodified from the OBS Studio repository
(`plugins/decklink/linux/decklink-sdk/`, https://github.com/obsproject/obs-studio),
which vendors them from the SDK. Only the files needed by the shim are kept.

## Licence

Copyright (c) Blackmagic Design. These interface files are distributed by
Blackmagic Design under the permissive licence reproduced at the top of each
file (a Boost-style licence: use, reproduction, modification and redistribution
are allowed provided the copyright notice and licence text are kept in source
copies). Each file keeps its licence header.

## Runtime

The DeckLink driver and runtime library (`libDeckLinkAPI.so`) are part of
Blackmagic Desktop Video, installed separately on the servers that have
DeckLink cards (see the install script). Without them the server starts
normally and reports that Desktop Video is not installed.
