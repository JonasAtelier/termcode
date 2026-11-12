### Install

With apt, on Debian or Ubuntu (new versions then come with `sudo apt upgrade`):

```sh
sudo curl -fsSLo /usr/share/keyrings/termcode.gpg https://jonasatelier.github.io/termcode-releases/termcode.gpg
echo "deb [signed-by=/usr/share/keyrings/termcode.gpg] https://jonasatelier.github.io/termcode-releases stable main" \
  | sudo tee /etc/apt/sources.list.d/termcode.list
sudo apt update && sudo apt install termcode
```

Or download the `.deb` for your machine below (`dpkg --print-architecture` says `amd64` or `arm64`), then `sudo apt install ./termcode_*.deb`.
