#!/usr/bin/env python3
import socket
import struct
import time
import argparse
import os
import uuid

# Constants
MAGIC_FTX1 = 0x31585446
FRAME_PACK = 4
FRAME_PACK_ACK = 5
FRAME_FINISH = 6
FRAME_PACK_V3 = 11
PACK_BUFFER_SIZE = 4 * 1024 * 1024  # 4MB pack size
DUMMY_CHUNK = b'\xAA' * 1024 # Reusable 1KB chunk for file data

def send_all(sock, data):
    total_sent = 0
    while total_sent < len(data):
        sent = sock.send(data[total_sent:])
        if sent == 0:
            raise RuntimeError("Socket connection broken")
        total_sent += sent

def create_pack_header_v2(record_count):
    return struct.pack('<I', record_count)

def create_pack_header_v3(pack_id, record_count):
    return struct.pack('<QI', pack_id, record_count)

def create_file_record(rel_path, data):
    path_bytes = rel_path.encode('utf-8')
    header = struct.pack('<H', len(path_bytes)) + path_bytes + struct.pack('<Q', len(data))
    return header + data

def send_frame_header(sock, frame_type, body_len):
    header = struct.pack('<IIQ', MAGIC_FTX1, frame_type, body_len)
    send_all(sock, header)

def recv_exact(sock, size):
    chunks = []
    received = 0
    while received < size:
        chunk = sock.recv(size - received)
        if not chunk:
            raise RuntimeError("Socket connection broken")
        chunks.append(chunk)
        received += len(chunk)
    return b''.join(chunks)

def read_frame(sock):
    header = recv_exact(sock, 16)
    magic, frame_type, body_len = struct.unpack('<IIQ', header)
    if magic != MAGIC_FTX1:
        raise RuntimeError(f"Invalid frame magic: 0x{magic:08X}")
    body = recv_exact(sock, body_len) if body_len else b''
    return frame_type, body

def send_handshake(sock, dest, use_v3):
    escaped = dest.replace("\\", "\\\\").replace("\"", "\\\"").replace("\n", "\\n").replace("\r", "\\r").replace("\t", "\\t")
    cmd = f"UPLOAD_V3 \"{escaped}\" DIRECT\n" if use_v3 else f"UPLOAD_V2 \"{escaped}\" DIRECT\n"
    print(f"Sending command: {cmd.strip()}")
    sock.sendall(cmd.encode('utf-8'))

    response = sock.recv(1024).decode('utf-8').strip()
    print(f"Server response: {response}")
    if response != "READY":
        raise RuntimeError(f"Server not ready: {response}")

def main():
    parser = argparse.ArgumentParser(description='Test PS5 Upload with many small files')
    parser.add_argument('ip', help='PS5 IP Address')
    parser.add_argument('--port', type=int, default=9113, help='Transfer Port')
    parser.add_argument('--num-files', type=int, default=300000, help='Total number of files to send')
    parser.add_argument('--file-size', type=int, default=1024, help='Size of each small file in bytes')
    parser.add_argument('--dest', default='/data/test_upload_many_small_files', help='Destination path on PS5')
    parser.add_argument('--force-v2', action='store_true', help='Force legacy V2 protocol')
    parser.add_argument('--ack-window', type=int, default=4, help='Max in-flight packs before waiting for ACK (V3 only)')
    args = parser.parse_args()

    print(f"Connecting to {args.ip}:{args.port}...")
    try:
        sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        sock.connect((args.ip, args.port))
        # Set a large send buffer to help with throughput
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_SNDBUF, 16 * 1024 * 1024)
        sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
    except Exception as e:
        print(f"Connection failed: {e}")
        return

    use_v3 = not args.force_v2
    try:
        send_handshake(sock, args.dest, use_v3)
    except Exception as e:
        if args.force_v2:
            print(f"Handshake failed: {e}")
            sock.close()
            return
        print(f"V3 handshake failed ({e}), retrying with V2...")
        sock.close()
        sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        sock.connect((args.ip, args.port))
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_SNDBUF, 16 * 1024 * 1024)
        sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        use_v3 = False
        try:
            send_handshake(sock, args.dest, False)
        except Exception as err:
            print(f"V2 handshake failed: {err}")
            sock.close()
            return

    files_sent = 0
    total_bytes_sent = 0
    start_time = time.time()
    
    print(f"Starting upload of {args.num_files} files of size {args.file_size} bytes each...")

    dummy_file_data = DUMMY_CHUNK * (args.file_size // len(DUMMY_CHUNK))
    if args.file_size % len(DUMMY_CHUNK) > 0:
        dummy_file_data += DUMMY_CHUNK[:args.file_size % len(DUMMY_CHUNK)]

    pack_id = 1
    inflight = {}

    while files_sent < args.num_files:
        pack_body = bytearray()
        if use_v3:
            pack_body.extend(create_pack_header_v3(pack_id, 0))
        else:
            pack_body.extend(create_pack_header_v2(0)) # Placeholder for record count
        
        records_in_pack = 0
        
        while files_sent < args.num_files:
            # Generate a unique path for each file to avoid overwriting
            file_path = f"file_{files_sent:06d}_{uuid.uuid4().hex[:8]}.bin"
            
            record = create_file_record(file_path, dummy_file_data)

            # Check if pack would exceed buffer size
            if len(pack_body) + len(record) > PACK_BUFFER_SIZE:
                if records_in_pack == 0:
                    print(f"Error: Single file record (size {len(record)}) is larger than pack buffer ({PACK_BUFFER_SIZE}).")
                    print("Increase PACK_BUFFER_SIZE or decrease file-size.")
                    sock.close()
                    return
                break # This pack is full, send it

            pack_body.extend(record)
            records_in_pack += 1
            files_sent += 1

        # Update the record count in the pack header
        if use_v3:
            pack_body[8:12] = struct.pack('<I', records_in_pack)
        else:
            pack_body[0:4] = struct.pack('<I', records_in_pack)
        
        # Send Frame Header
        send_frame_header(sock, FRAME_PACK_V3 if use_v3 else FRAME_PACK, len(pack_body))
        
        # Send Pack Body
        send_all(sock, pack_body)
        total_bytes_sent += len(pack_body)
        if use_v3:
            inflight[pack_id] = time.time()
            pack_id += 1
            while len(inflight) >= max(1, args.ack_window):
                frame_type, body = read_frame(sock)
                if frame_type == FRAME_PACK_ACK and len(body) >= 8:
                    ack_id = struct.unpack('<Q', body[:8])[0]
                    inflight.pop(ack_id, None)
                else:
                    print(f"\nUnexpected frame type {frame_type} while waiting for ACK.")
                    break
        
        # Progress update
        elapsed = time.time() - start_time
        if elapsed > 0:
            speed = (total_bytes_sent / 1024 / 1024) / elapsed
            file_rate = files_sent / elapsed
            print(f"\rFiles Sent: {files_sent}/{args.num_files} | Rate: {file_rate:.0f} files/s | Speed: {speed:.2f} MB/s", end='')

    print("\nSending FINISH frame...")
    send_frame_header(sock, FRAME_FINISH, 0)
    
    print("Waiting for final confirmation...")
    try:
        # Set a timeout for the final response
        sock.settimeout(60.0) # 60 seconds
        if use_v3:
            while inflight:
                frame_type, body = read_frame(sock)
                if frame_type == FRAME_PACK_ACK and len(body) >= 8:
                    ack_id = struct.unpack('<Q', body[:8])[0]
                    inflight.pop(ack_id, None)
                else:
                    print(f"Unexpected frame type {frame_type} while waiting for ACK.")
                    break
        final_resp = sock.recv(1024).decode('utf-8').strip()
        print(f"Final Response: {final_resp}")
    except socket.timeout:
        print("Timeout: Did not receive final confirmation from server.")
    except Exception as e:
        print(f"Error reading response: {e}")

    sock.close() 
    
    total_time = time.time() - start_time
    avg_speed = (total_bytes_sent / 1024 / 1024) / total_time
    print(f"Done! Sent {files_sent} files in {total_time:.2f} seconds. Average Speed: {avg_speed:.2f} MB/s")

if __name__ == "__main__":
    main()
