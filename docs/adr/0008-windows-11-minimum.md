---
id: ADR-0008
title: "Windows 11 22H2 minimum"
kind: adr
status: accepted
decided: 2026-10-13
rev: 1
review_every: 180d
depends_on: [ext:mf-virtual-camera, ext:win10-lifecycle, ext:win-graphics-capture, ext:wasapi-process-loopback]
verified: {date: 2026-10-13, by: "agent:claude", rev: 1, note: "Initial version, recorded from design Q&A with the maintainer"}
---

# 0008: Windows 11 22H2 minimum


## Context
Windows 10 Home/Pro reached end of support on October 14, 2025 [[ext:win10-lifecycle]].
A virtual camera on Windows 10 requires a DirectShow filter DLL registered by an admin
installer. The OS-native `MFCreateVirtualCamera` [[ext:mf-virtual-camera]] requires
Windows build 22000 [[fact:mf-vcam-min-build]] (Windows 11 21H2). Per-process
audio loopback is available from build 20348 [[ext:wasapi-process-loopback]].

## Decision
Minimum Windows version is Windows 11 22H2 [[fact:min-windows]]. This is a support-policy
floor above the API minimum (build 22000), chosen to stay on a serviced Windows release.
The virtual camera uses `MFCreateVirtualCamera`. Capture uses Windows.Graphics.Capture
[[ext:win-graphics-capture]]; audio uses WASAPI including process loopback.

## Consequences
- No COM filter DLL or driver; less `unsafe` and a simpler installer.
- Windows 10 users are not supported.
