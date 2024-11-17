# Bluefang Player

A simple program that can turn your computer into a bluetooth speaker.


## Running

This program uses its own bluetooth stack ([bluefang](https://github.com/sidit77/bluefang)) instead of the system's bluetooth stack.

For this reason it must be able to take full control of the bluetooth adapter. For this reason some platform dependent setup is required:
* **Windows**: The default driver must be replaced with WinUSB using a tool like [Zadig](https://zadig.akeo.ie/).
* **Linux**: The user running this executable must have access to the Bluetooth device. This can be achieved by adding the correct udev rules or by running the executable as root.

## Building

This project requires a working [Rust Toolchain](https://rustup.rs/).

Afterwards, the project can be built using the standard cargo commands:
```sh
cargo build --release
```

## License

MIT License
