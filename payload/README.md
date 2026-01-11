# PS5 Upload - Payload

This is the code that runs on your PS5. Think of it as the "receiver". It sits quietly in the background, waiting for the client to send files, and writes them to the disk as fast as possible.

## How to use it

You don't "install" this permanently. You load it whenever you want to transfer files.

### Sending the Payload
You need to send the `ps5upload.elf` file to your console's payload loader (usually running on port 9020 or 9021).

**Using Netcat (Linux/Mac/Windows WSL):**
Replace `192.168.1.xxx` with your PS5's IP.
```bash
nc -w 1 192.168.1.xxx 9021 < ps5upload.elf
```

**Using GUI Tools:**
Any "Payload Sender" app will work. Just pick `ps5upload.elf` as the file and hit send.

### How do I know it's working?
Look at your TV. You should see a notification bubble:
> **PS5 Upload Server - Ready on port 9113**

Once you see that, you can open the Client on your PC and connect.

## For Developers: Building

If you want to modify the C code or compile it yourself, you need the [PS5 Payload SDK](https://github.com/ps5-payload-dev/sdk).

1.  **Environment:**
    Make sure `PS5_PAYLOAD_SDK` is set in your environment variables.
    ```bash
    export PS5_PAYLOAD_SDK=/path/to/sdk
    ```

2.  **Compile:**
    ```bash
    make
    ```
    This spits out a fresh `ps5upload.elf`.

## Configuration
Check `config.h` if you want to tweak things like:
*   `SERVER_PORT` (Default: 9113) - Change this if you have port conflicts.
*   `BUFFER_SIZE` - Tuning for specific network environments.