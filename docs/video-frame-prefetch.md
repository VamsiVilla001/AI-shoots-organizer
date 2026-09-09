# Overlapping video decoding and AI

`videoFramePrefetch` defaults to true and can be disabled in Settings > Video
for comparison. It applies to newly started video jobs. The original sampling
plan, timestamps, resolution, orientation, face models and tracking order are
unchanged.

A helper thread prepares the next sampled frame while the AI worker processes
the current one for videos up to one minute. For longer videos, the frame plan
is split into two to four ordered segments that perform accurate timestamp
seeks concurrently. A process-wide pool limits all long-video work to ten
FFmpeg tasks, including when several AI workers run together. No temporary
video or frame files are added.

The consumer runs on the AI worker's original thread. Frames and decode errors
arrive in planned order. Short-video prefetch remains one frame ahead; long
video segment results are reordered before AI and database writes. Pausing
retains the existing behaviour: active files finish and save; new files in
that shoot do not start.

Completed video log entries include `decoder_segments`, `decode_ms`,
`frame_wait_ms`, `frame_analysis_ms` and `total_ms`. For a long video, decode is
the wall time of all parallel segments. For a short video, decode
and analysis overlap and must not be added to estimate elapsed time. Total also
includes planning and database preparation/finalisation.

Tests cover ordered outputs including decode errors, actual producer/consumer
overlap, the one-frame preparation bound, segment selection, empty input and
early-error cleanup. Hardware speedup and recognition-output equivalence still
need a representative on/off run; more concurrent decoding may compete for CPU
or GPU resources.
