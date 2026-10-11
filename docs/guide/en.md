# TunnelHop user guide

English · [Українська](uk.md) · [Русский](ru.md)

TunnelHop connects your Windows PC to your VPN provider's servers, switches between them on
a schedule, and blocks all traffic while it switches, so nothing leaves the PC outside the
tunnel. This guide takes you from installing it to moving your setup to another PC, at home
or in an office.

## 1. What you need

- Windows 10 or 11, 64-bit, and an account that can approve an administrator prompt once,
  for the installation.
- *WireGuard for Windows* 0.5.3 or newer, from <https://www.wireguard.com/install/>, with its
  default options. TunnelHop drives it; it does not replace it.
- Your provider's WireGuard configuration files (`.conf`). Most providers offer them in
  their account pages, often under *Manual setup* or *Router*. Download one file per server
  you want to use.

  Or a TunnelHop bundle (a `.zip`) that someone exported for you (section 7).

### Why WireGuard

Use WireGuard whenever your provider offers it. Each `.conf` file carries its own keys, so
there is nothing more to sign in with. It connects within seconds, which matters because
traffic is blocked for as long as a switch between servers takes. And it needs one program,
with nothing to choose between.

### OpenVPN, if WireGuard is not an option

TunnelHop also takes OpenVPN files (`.ovpn`), for providers or servers without WireGuard.
They need *OpenVPN Community* 2.6 or newer, from <https://openvpn.net/community-downloads/>
(the Windows 64-bit MSI installer). *OpenVPN Connect*, which openvpn.net and the Microsoft
Store offer first, is a different program and does not work with TunnelHop; it can stay
installed next to the Community one. Most providers' OpenVPN servers also ask for a username
and password (section 5).

A bundle can hold WireGuard and OpenVPN servers together.

## 2. Install TunnelHop

1. Open <https://github.com/matasarei/tunnelhop/releases/latest> and download
   `TunnelHop_<version>_x64-setup.exe`. To check the download, compare its SHA-256 with the
   line in `SHA256SUMS.txt` next to it (`Get-FileHash <file>` in PowerShell).
2. Run it. The installer is not code-signed yet, so Windows may show *Windows protected your
   PC*: choose *More info*, then *Run anyway*.
3. Approve the administrator prompt. The installer registers the TunnelHop service, which
   does the privileged work. The app itself never runs as administrator.
4. When it finishes, TunnelHop starts and its icon appears in the notification area (by the
   clock; it may be under the ^ arrow). It also starts with Windows from now on, straight
   to that icon. To stop that, turn TunnelHop off under *Settings → Apps → Startup*.

Click the icon to open the window, or right-click it and choose **Open Dashboard**. Closing
the window only hides it; TunnelHop keeps running in the notification area.

The window and the menu follow your Windows display language. Messages that come from the
service itself, such as why a connection failed or why a file was refused, are always in
English.

## 3. Add your servers

The first time, the window says it has no servers yet.

1. Choose **Add servers…** and pick your provider's `.conf` files. You can also drag the
   files onto the window, or a `.zip` that holds only such files.
2. A dialog lists the files. Give the collection a name under **Bundle name** (the default is
   **My servers**), adjust the name of each server if you like, and choose **Create**.

TunnelHop calls such a collection of servers a *bundle*. The new bundle is now **In use**,
and its servers are listed in the window.

Every file is checked before anything is installed. A file that could run a program on your
PC is refused, and the window lists each problem with the file and line it is on. Fix or
drop that file and try again.

To add more servers later, choose **Add server…** above the server list, or drag the files
onto the window. To remove one, use the **Remove** button next to it.

## 4. Or import a bundle

If someone gave you a TunnelHop bundle (a `.zip` exported as in section 7):

1. Choose **Import bundle…** under **Bundles**, and pick the file. Dragging it onto the
   window works too.
2. If it is password-protected, the window shows **Password-protected bundle**. Enter the
   password you were given and choose **Import**.

If you were not connected, the imported bundle is put **In use** straight away. If you
were, a bar offers to switch to it, and it is listed under **Bundles** with a **Switch**
button. Importing the same file again does nothing harmful: it is recognised as already
installed.

## 5. OpenVPN servers: sign in

*WireGuard servers skip this section.*

Many providers' OpenVPN servers ask for a username and password. When a bundle needs them,
TunnelHop asks right after adding or importing it.

1. Enter the *OpenVPN* username and password from your provider. These are *not* the email
   and password you use to sign in to your account with them: providers list them
   separately. Proton VPN, for example, shows them under *Account → OpenVPN / IKEv2
   username*.
2. Keep **Use for every OpenVPN server in this bundle** ticked, unless your servers use
   different sign-ins, and choose **Save**.

The username and password are stored by the TunnelHop service, encrypted for this PC; the
app keeps no copy. If you chose **Not now**, a server waiting for them is marked
**Needs sign-in**, and its **Sign in…** button asks again. If the server later refuses
them, TunnelHop asks again rather than retrying.

A server marked **Unavailable**, WireGuard or OpenVPN, needs a program that is missing or too
old: choose **Why?** for which one and where to get it (section 1). The mark goes away a few
seconds after the program is installed.

## 6. Connect

- **Connect** picks a server at random from the bundle in use. To use a particular server,
  use the **Connect** button next to it in the list.
- **Rotate now** switches to another server straight away. With automatic rotation on,
  TunnelHop also switches on its own: right-click the icon to turn **Auto-Rotation** on or
  off and to choose the **Interval**.
- **Disconnect & unblock** disconnects and lets traffic flow normally again, outside the VPN.

While TunnelHop switches servers, or if a server cannot be reached, all traffic is blocked,
your local network included. The window then shows **Blocked**. This is on purpose: your
traffic never goes out unprotected without you choosing so. TunnelHop tries other servers
by itself; you can also **Connect** again, or **Disconnect & unblock**.

## 7. Move your setup to another PC (home or office)

Export the bundle on a PC where it already works, then import it on each other PC. The
other PCs need no files from your provider.

### On the PC that has the bundle

1. Under **Bundles**, choose **Export** next to the bundle.
2. For OpenVPN servers only: leave **Include OpenVPN sign-ins** on, so that the other PCs
   connect without asking for them. Turn it off if each person should enter their own.
3. Enter a password in **Password (recommended)** and again in **Repeat the password**. The
   file is then encrypted with AES-256. TunnelHop asks for this password when importing it.
   Windows Explorer cannot open such a file; that is expected.
4. Choose **Export**, and where to save the file. Without a password the button reads
   **Export without password**: the file is then readable by anyone who gets it.

The file holds the servers' private keys and, if included, the sign-ins: anyone who has it
and the password can connect through your VPN account. Carry it on a USB stick, or share it
through a password manager or a private file share. Do not send the file and its password
together, and never in a public place.

### On each other PC

1. Install WireGuard for Windows (and OpenVPN Community, if the bundle has OpenVPN servers),
   as in section 1, then TunnelHop (section 2).
2. Import the file (section 4) and enter its password.
3. Choose **Connect**.

### For an office

- Every account signed in to a PC can export the bundles installed there, keys and sign-ins
  included. Give each person their own Windows account, and keep the bundle off PCs that
  people you don't trust can sign in to.
- After changing the servers (section 3), export again and import the new file on each PC.
  The old bundle stays listed there until you **Delete** it.
- OpenVPN sign-ins entered on a PC (section 5) stay on that PC. Export with
  **Include OpenVPN sign-ins** to pass them on.

## 8. If the internet stays blocked

The block is meant to hold until a tunnel is up, and it survives a crash or a restart of
Windows on purpose. To lift it:

1. Choose **Disconnect & unblock** in the window, or in the menu of the icon.
2. Or choose **Exit** in that menu: TunnelHop disconnects, lifts the block, and closes.
3. If neither is available (the window says **Service not running**, or the icon is gone),
   open a terminal *as administrator* and run:

   ```
   "C:\Program Files\TunnelHop\th-service.exe" disengage
   ```

   Restarting Windows alone does not lift the block.

## 9. Uninstall

Open *Settings → Apps → Installed apps*, find TunnelHop, and choose *Uninstall* (on
Windows 10: *Apps & features*). Uninstalling disconnects, lifts the block, removes the
TunnelHop service, and deletes every bundle and stored sign-in from the PC. WireGuard (and
OpenVPN, if you installed it) stays; remove it separately if you no longer need it.
