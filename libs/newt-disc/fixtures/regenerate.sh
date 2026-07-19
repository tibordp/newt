#!/bin/bash
# Regenerates the disc-image fixtures. macOS-only (hdiutil, newfs_udf);
# the Rock Ridge image is built with pycdlib via uv.
set -euo pipefail
cd "$(dirname "$0")"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# --- source content tree ---------------------------------------------------
content="$work/content"
mkdir -p "$content/sub/deeper"
printf 'Hello from the disc image!\n' > "$content/hello.txt"
printf 'nested\n' > "$content/sub/nested.txt"
printf 'deep file content\n' > "$content/sub/deeper/deep.txt"
printf 'hidden\n' > "$content/.hidden.txt"
printf 'unicode\n' > "$content/Ünïcødé nämé.txt"
python3 -c "
import random, sys
r = random.Random(42)
open(sys.argv[1], 'wb').write(bytes(r.randrange(256) for _ in range(65536)))
" "$content/big.bin"

# --- ISO 9660 / Joliet / UDF 1.x / hybrid ----------------------------------
hdiutil makehybrid -iso -default-volume-name NEWTTEST -o "$work/plain.iso" "$content"
hdiutil makehybrid -iso -joliet -default-volume-name NEWTTEST -o "$work/joliet.iso" "$content"
hdiutil makehybrid -udf -udf-version 1.50 -default-volume-name NEWTTEST -o "$work/udf150.iso" "$content"
hdiutil makehybrid -iso -joliet -udf -udf-version 1.02 -default-volume-name NEWTTEST -o "$work/hybrid.iso" "$content"

# --- UDF 2.50 (metadata partition) -----------------------------------------
hdiutil create -size 8m -layout NONE -o "$work/blank250.dmg"
dev=$(hdiutil attach -nomount "$work/blank250.dmg" | awk '{print $1}' | head -1)
newfs_udf -r 2.50 -v NEWTUDF250 "$dev"
hdiutil detach "$dev"
dev=$(hdiutil attach "$work/blank250.dmg" | awk '{print $1}' | head -1)
vol=/Volumes/NEWTUDF250
cp -R "$content/" "$vol/"
ln -s hello.txt "$vol/link_to_hello"
ln -s sub/deeper "$vol/link_to_deeper"
hdiutil detach "$dev"
mv "$work/blank250.dmg" "$work/udf250.iso"

# --- Rock Ridge (pycdlib) ---------------------------------------------------
cat > "$work/gen_rr.py" <<'EOF'
import pycdlib, io, random, sys

iso = pycdlib.PyCdlib()
iso.new(interchange_level=1, rock_ridge='1.09', vol_ident='NEWTRR')

hello = b'Hello from the disc image!\n'
iso.add_fp(io.BytesIO(hello), len(hello), '/HELLO.TXT;1', rr_name='hello.txt')
r = random.Random(42)
big = bytes(r.randrange(256) for _ in range(65536))
iso.add_fp(io.BytesIO(big), len(big), '/BIG.BIN;1', rr_name='big.bin')
iso.add_directory('/SUB', rr_name='sub')
nested = b'nested\n'
iso.add_fp(io.BytesIO(nested), len(nested), '/SUB/NESTED.TXT;1', rr_name='nested.txt')
iso.add_directory('/SUB/DEEPER', rr_name='deeper')
deep = b'deep file content\n'
iso.add_fp(io.BytesIO(deep), len(deep), '/SUB/DEEPER/DEEP.TXT;1', rr_name='deep.txt')
long_name = 'a_rather_long_rock_ridge_file_name_that_exceeds_iso_level_one.txt'
data = b'long name\n'
iso.add_fp(io.BytesIO(data), len(data), '/LONGNAME.TXT;1', rr_name=long_name)
iso.add_symlink('/LINKHELLO.;1', rr_symlink_name='link_to_hello', rr_path='hello.txt')
iso.add_symlink('/LINKDEEP.;1', rr_symlink_name='link_to_deeper', rr_path='sub/deeper')
iso.add_symlink('/LINKABS.;1', rr_symlink_name='link_abs', rr_path='/sub/nested.txt')

# Force SUSP CE continuation areas: a ~200-char name plus a ~200-char
# symlink target cannot fit the 255-byte directory record.
very_long = 'prefix_' + 'x' * 180 + '_suffix.txt'
data2 = b'very long name\n'
iso.add_fp(io.BytesIO(data2), len(data2), '/VERYLONG.TXT;1', rr_name=very_long)
long_target = 'sub/' + '/'.join(['seg' + str(i) * 20 for i in range(8)])
iso.add_symlink('/LINKLONG.;1', rr_symlink_name='link_long', rr_path=long_target)
iso.write(sys.argv[1])
iso.close()
EOF
uv run --with pycdlib python "$work/gen_rr.py" "$work/rockridge.iso"

# --- compress into the crate ------------------------------------------------
for f in plain joliet rockridge udf150 udf250 hybrid; do
  gzip -9 -c "$work/$f.iso" > "$f.iso.gz"
done
echo done
