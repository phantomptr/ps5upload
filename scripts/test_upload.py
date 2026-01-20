#!/usr/bin/env python3
import socket
import struct
import time
import argparse
import os

# Constants
MAGIC_FTX1 = 0x31585446
FRAME_PACK = 4
FRAME_FINISH = 6
PACK_BUFFER_SIZE = 16 * 1024 * 1024  # 16MB
CHUNK_SIZE = 1024 * 1024  # 1MB chunks for generation

def send_all(sock, data):
    total_sent = 0
    while total_sent < len(data):
        sent = sock.send(data[total_sent:])
        if sent == 0:
            raise RuntimeError("Socket connection broken")
        total_sent += sent

def create_pack_header(record_count):
    return struct.pack('<I', record_count)

def create_file_record_header(rel_path, data_len):
    path_bytes = rel_path.encode('utf-8')
    return struct.pack('<H', len(path_bytes)) + path_bytes + struct.pack('<Q', data_len)

def send_frame_header(sock, frame_type, body_len):
    # Magic (4), Type (4), Len (8)
    header = struct.pack('<IIQ', MAGIC_FTX1, frame_type, body_len)
    send_all(sock, header)

def main():
    parser = argparse.ArgumentParser(description='Test PS5 Upload Speed')
    parser.add_argument('ip', help='PS5 IP Address')
    parser.add_argument('--port', type=int, default=9113, help='Transfer Port')
    parser.add_argument('--size', type=int, default=1024, help='Total size to upload in MB')
    parser.add_argument('--dest', default='/data/test_upload', help='Destination path on PS5')
    args = parser.parse_args()

    print(f"Connecting to {args.ip}:{args.port}...")
    try:
        sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        sock.connect((args.ip, args.port))
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_SNDBUF, 16 * 1024 * 1024)
        sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
    except Exception as e:
        print(f"Connection failed: {e}")
        return

    # Handshake
    cmd = f"UPLOAD_V2 {args.dest} DIRECT\n"
    print(f"Sending command: {cmd.strip()}")
    sock.sendall(cmd.encode('utf-8'))

    # Wait for READY
    response = sock.recv(1024).decode('utf-8').strip()
    print(f"Server response: {response}")
    if response != "READY":
        print("Server not ready.")
        return

    total_bytes_to_send = args.size * 1024 * 1024
    bytes_sent_total = 0
    start_time = time.time()
    
    # We will simulate sending one large file split into packs
    filename = "dummy_big_file.bin"
    
    # Construct a pack
    # For simplicity in this test script, we will send one file per pack or split it?
    # The protocol supports multiple files per pack, or one file split across packs?
    # Looking at core/src/transfer.rs: "pack.add_record".
    # It seems the protocol expects full files within packs? 
    # "pack.add_record" adds path length, path, data length, data.
    # It does NOT seem to support splitting a single file record across multiple packs easily 
    # unless we treat them as separate files on the receiver or if the receiver appends.
    # payload/transfer.c: write_pack_worker uses "ab" (append binary) if !current_fp.
    # So yes, we can split a file across packs by using the same filename.
    
    current_bytes_generated = 0
    
    print(f"Starting upload of {args.size} MB...")

    while bytes_sent_total < total_bytes_to_send:
        # Determine pack size
        remaining = total_bytes_to_send - bytes_sent_total
        payload_size = min(PACK_BUFFER_SIZE - 1024, remaining) # Leave room for headers
        
        # Build Pack Body
        # 1. Record Count (1)
        pack_body = bytearray()
        pack_body.extend(create_pack_header(1))
        
        # 2. Record Header
        # We are sending a chunk of the file
        record_header = create_file_record_header(filename, payload_size)
        pack_body.extend(record_header)
        
        # 3. Data (Dummy)
        # We just allocate zeroed bytes for speed testing, or pattern
        # To avoid massive memory usage in python, we can just send the header frame 
        # and then stream the body data directly to the socket
        
        frame_len = len(pack_body) + payload_size
        
        # Send Frame Header
        send_frame_header(sock, FRAME_PACK, frame_len)
        
        # Send Pack Info (Record Count + Record Header)
        send_all(sock, pack_body)
        
        # Send Payload Data in chunks
        chunk_sent = 0
        dummy_chunk = b'\xAA' * min(CHUNK_SIZE, payload_size)
        
        while chunk_sent < payload_size:
            to_send = min(len(dummy_chunk), payload_size - chunk_sent)
            if to_send != len(dummy_chunk):
                 send_all(sock, dummy_chunk[:to_send])
            else:
                 send_all(sock, dummy_chunk)
            chunk_sent += to_send
            
            # Progress update within pack
            current_total = bytes_sent_total + chunk_sent
            elapsed = time.time() - start_time
            if elapsed > 0:
                speed = (current_total / 1024 / 1024) / elapsed
                print(f"\rProgress: {current_total/1024/1024:.2f} MB / {args.size:.2f} MB ({speed:.2f} MB/s)", end='')

        bytes_sent_total += payload_size

    print("\nSending FINISH frame...")
    send_frame_header(sock, FRAME_FINISH, 0)
    
    print("Waiting for final confirmation...")
    try:
        final_resp = sock.recv(1024).decode('utf-8').strip()
        print(f"Final Response: {final_resp}")
    except Exception as e:
        print(f"Error reading response: {e}")

    sock.close() 
    
    total_time = time.time() - start_time
    avg_speed = (bytes_sent_total / 1024 / 1024) / total_time
    print(f"Done! Average Speed: {avg_speed:.2f} MB/s")

if __name__ == "__main__":
    main()
