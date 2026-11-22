# Autoplay and recommendations

Navia's recommendation engine is in `crates/navia-core/src/engine/`. It picks what plays when the queue ends
(Autoplay), builds song and artist radio, and fills mixes. Autoplay has two modes: **Navia** uses this engine,
and **Server** plays the server's similar songs in order.

The engine uses what the server provides and what you do in Navia. Every source is optional. If the server has
no Last.fm, no external agents or no ratings, the engine uses what's left.

## Sources

- `getSimilarSongs2` by song, weighted by rank since the best matches come first. Never by album or playlist,
  which returns random songs. The server tops an agent's answer up with songs that change on every call, so
  Navia asks twice and keeps only the part both answers share.
- `getTopSongs`, by artist id when the server supports it, for popularity.
- Similar artists from `getArtistInfo2`, combined with Deezer's related artists when Deezer is on. Artists both
  agree on count for more.
- Stars, play counts, last played and ratings, when the server has them.
- Your own playlists, as songs that go together. Smart playlists are skipped: they're rules, not taste, and
  fetching one makes the server recalculate it.
- Never `getRandomSongs`. A random pick from the local library costs nothing.
- Deezer, only when you turn it on: artist radio, related artists, top tracks and BPM for songs without a BPM tag.
  Deezer songs are matched to your library by title and main artist, then by duration within about 3 seconds, and
  by ISRC when that's still ambiguous.
- ListenBrainz, under the same switch: similar recordings for the songs you start, by the song's MusicBrainz id
  or by artist and title. Anonymous, at most two requests a second, and any failure is skipped.

## Deezer limits

The whole app shares one Deezer client: 5 requests a second, at most 300 an hour, and duplicate requests merged.
After a quota error it waits 10 seconds, doubling up to 10 minutes, and uses cached answers meanwhile.

| Cached | For |
| --- | --- |
| Artist ids | 90 days, misses 7 days |
| Related artists, top tracks | 7 days |
| Artist radio | 1 hour, keeping the last 3 |
| Track details | kept |

The cache is capped at about 20 MB and drops the least recently used entries first.

## What Navia learns

- **Play log:** every play, with how far it got, how it ended (finished, skipped or replaced), whether you or
  Autoplay picked it, and a session id. A new session starts after 30 minutes of silence.
- **Appeal:** for each song, finished plays against skips in the first 30 seconds, fading over months. A new song
  starts from its artist's appeal, then its genre's, then the server's plays, stars and rating.
- **Neighbours:** songs finished within about 5 songs of each other in a session, the top 50 per song. Autoplay's
  own picks count for less.

## Scoring

**Score = relatedness × (0.65 + 0.35 × appeal)**

Appeal only reorders songs that are about equally related, so a favorite never beats a clearly closer match.

Relatedness adds up these parts, each weighted by how much evidence there is:

| Part | Weight |
| --- | --- |
| Neighbours and playlists | 0.35 |
| The server's similar songs | 0.20 |
| Same or similar artist | 0.20 |
| Genre | 0.20 |
| Era: 1 for the same year, ½ at 8 years apart, 0 at 16 | 0.15 |

Songs scoring under about 0.35 are dropped. Only the main seed's own lists (the server's and Deezer's radio) can
fill in when there are too few.

Appeal is sampled (Thompson sampling) and multiplied by freshness, which also makes room for new songs. Early
skips lower it.

## Seeds

Your latest pick leads, at weight 1: the song Autoplay follows, or the song a radio starts from. Up to four more
seeds count 0.2 each: songs you played at least halfway this session, and other recent picks. They only count
when they agree with the lead, by similar artists, genre or artist. Radio from a song uses that song alone.

## Order

Autoplay keeps 25 songs ahead, spread across artists: at most 2 by one artist in those 25, never two in a row,
and no album again within 3 songs. The artist you started from may come up at most twice in the first 10. The
rules loosen only when nothing else fits, so a small library still gets a full list.

## Skips

- One early skip means "not now", and it fades.
- Early skips on separate days mean you don't like it, until you star it or play it yourself.
- Late skips barely count.
- Two early skips in a row restart from your last finished songs and lean toward familiar ones.
- Nothing is banned for good.

## Testing

`autoplay_replay`, `autoplay_compare` and `fatigue_replay` in `crates/navia-core/examples/` replay play logs
offline. They measure how many of the songs you played next are in the top 20, and how well finished and skipped
songs are told apart.
