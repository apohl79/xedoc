"""IPv4 addresses and subnet broadcasts from operating-system interfaces."""

import ctypes
import ipaddress
import os
import socket
import sys


def ipv4_broadcasts() -> tuple[str, ...]:
    if os.name == "nt":
        return _windows_broadcasts()
    return _unix_ipv4("broadcast")


def ipv4_addresses() -> tuple[str, ...]:
    return _unix_ipv4("address")


def _unix_ipv4(address_field: str) -> tuple[str, ...]:
    class Interface(ctypes.Structure):
        pass

    Interface._fields_ = [
        ("next", ctypes.POINTER(Interface)),
        ("name", ctypes.c_char_p),
        ("flags", ctypes.c_uint),
        ("address", ctypes.c_void_p),
        ("netmask", ctypes.c_void_p),
        ("broadcast", ctypes.c_void_p),
        ("data", ctypes.c_void_p),
    ]
    library = ctypes.CDLL(None, use_errno=True)
    library.getifaddrs.argtypes = [ctypes.POINTER(ctypes.POINTER(Interface))]
    library.getifaddrs.restype = ctypes.c_int
    library.freeifaddrs.argtypes = [ctypes.POINTER(Interface)]
    library.freeifaddrs.restype = None
    first = ctypes.POINTER(Interface)()
    if library.getifaddrs(ctypes.byref(first)) != 0:
        raise OSError(ctypes.get_errno(), "Cannot enumerate network interfaces")
    addresses = set()
    required_flags = 0x3 if address_field == "broadcast" else 0x1
    try:
        current = first
        for _ in range(256):
            if not current:
                break
            interface = current.contents
            current = interface.next
            if (
                interface.flags & required_flags != required_flags
                or interface.flags & 0x8
                or not interface.address
                or not getattr(interface, address_field)
            ):
                continue
            address = ctypes.string_at(interface.address, 8)
            family = (
                address[1]
                if sys.platform == "darwin"
                else int.from_bytes(address[:2], sys.byteorder)
            )
            if family == socket.AF_INET:
                value = ctypes.string_at(getattr(interface, address_field), 8)[4:8]
                addresses.add(socket.inet_ntoa(value))
    finally:
        library.freeifaddrs(first)
    return tuple(sorted(addresses))


def _windows_broadcasts() -> tuple[str, ...]:
    from ctypes import wintypes

    class AddressRow(ctypes.Structure):
        _fields_ = [
            ("address", wintypes.DWORD),
            ("index", wintypes.DWORD),
            ("mask", wintypes.DWORD),
            ("broadcast", wintypes.DWORD),
            ("reassembly", wintypes.DWORD),
            ("unused", wintypes.WORD),
            ("type", wintypes.WORD),
        ]

    get_table = ctypes.WinDLL("iphlpapi").GetIpAddrTable
    get_table.argtypes = [
        ctypes.c_void_p,
        ctypes.POINTER(wintypes.ULONG),
        wintypes.BOOL,
    ]
    get_table.restype = wintypes.DWORD
    size = wintypes.ULONG()
    if get_table(None, ctypes.byref(size), False) != 122 or size.value > 65_536:
        raise OSError("Cannot enumerate network interfaces")
    buffer = ctypes.create_string_buffer(size.value)
    if get_table(buffer, ctypes.byref(size), False) != 0:
        raise OSError("Cannot enumerate network interfaces")
    count = wintypes.DWORD.from_buffer(buffer).value
    if count > 256 or 4 + count * ctypes.sizeof(AddressRow) > len(buffer):
        raise OSError("Invalid network interface table")
    broadcasts = set()
    for index in range(count):
        row = AddressRow.from_buffer(buffer, 4 + index * ctypes.sizeof(AddressRow))
        address = ipaddress.IPv4Address(row.address.to_bytes(4, "little"))
        mask = socket.inet_ntoa(row.mask.to_bytes(4, "little"))
        if address.is_loopback or address.is_unspecified:
            continue
        network = ipaddress.IPv4Network(f"{address}/{mask}", strict=False)
        if network.prefixlen < 31:
            broadcasts.add(str(network.broadcast_address))
    return tuple(sorted(broadcasts))
