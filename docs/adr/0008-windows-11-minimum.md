# 0008: Windows 11 22H2 minimum

- Status: Accepted
- Date: 2026-10-13

## Context
Windows 10 reached end of support in October 2025. A virtual camera on Windows 10
requires a DirectShow filter DLL registered by an admin installer.

## Decision
Minimum Windows version is Windows 11 22H2. The virtual camera uses
`MFCreateVirtualCamera`. Capture uses Windows.Graphics.Capture; audio uses WASAPI
including process loopback.

## Consequences
- No COM filter DLL or driver; less `unsafe` and a simpler installer.
- Windows 10 users are not supported.
