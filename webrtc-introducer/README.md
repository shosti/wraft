# WebRTC Introducer

This is a very basic WebRTC signaling server for use with WRaft. Basically, it
just uses WebSockets to announce the presence of nodes for a given session key,
as well as forwarding Join/ICE server requests to proper nodes.
