import io.netty.buffer.Unpooled;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.file.Files;
import java.nio.file.Path;
import net.minecraft.core.IdMapper;
import net.minecraft.network.FriendlyByteBuf;
import net.minecraft.world.level.chunk.PalettedContainer;

/** Independent fixtures serialized by the installed Minecraft 1.21.1 classes.
 * Uses synthetic registry identities to exercise large modded IDs without
 * executing mods or opening a saved world. No Minecraft encoder is copied.
 */
public class PaletteReference {
    public static void main(String[] args) throws Exception {
        if (args.length != 1) throw new IllegalArgumentException("Expected output directory");
        Path output = Path.of(args[0]);
        Files.createDirectories(output);
        IdMapper<Object> blocks = new IdMapper<>(65536);
        Object[] blockValues = new Object[65536];
        for (int i=0;i<blockValues.length;i++) { blockValues[i]=new Object(); blocks.add(blockValues[i]); }
        IdMapper<Object> biomes = new IdMapper<>(64);
        Object[] biomeValues = new Object[64];
        for (int i=0;i<biomeValues.length;i++) { biomeValues[i]=new Object(); biomes.add(biomeValues[i]); }
        FriendlyByteBuf wire = new FriendlyByteBuf(Unpooled.buffer());
        ByteBuffer expectedBlocks = ByteBuffer.allocate(6*4096*4).order(ByteOrder.LITTLE_ENDIAN);
        ByteBuffer expectedBiomes = ByteBuffer.allocate(6*64*4).order(ByteOrder.LITTLE_ENDIAN);
        int[] variants = {1,20,256,257,4096,1};
        int[] biomeVariants = {1,3,8,9,64,1};
        for (int section=0;section<6;section++) {
            int base = section==0 ? 50000 : section==5 ? 0 : 17000;
            PalettedContainer<Object> states = new PalettedContainer<>(blocks, blockValues[base], PalettedContainer.Strategy.SECTION_STATES);
            for (int y=0;y<16;y++) for (int z=0;z<16;z++) for (int x=0;x<16;x++) {
                int index=(y<<8)|(z<<4)|x;
                int id=base+index%variants[section];
                states.set(x,y,z,blockValues[id]);
                expectedBlocks.putInt(id);
            }
            PalettedContainer<Object> biomeStates = new PalettedContainer<>(biomes, biomeValues[0], PalettedContainer.Strategy.SECTION_BIOMES);
            for (int y=0;y<4;y++) for (int z=0;z<4;z++) for (int x=0;x<4;x++) {
                int id=((y<<4)|(z<<2)|x)%biomeVariants[section];
                biomeStates.set(x,y,z,biomeValues[id]);
                expectedBiomes.putInt(id);
            }
            wire.writeShort(section==5 ? 0 : 4096);
            states.write(wire);
            biomeStates.write(wire);
        }
        byte[] data = new byte[wire.readableBytes()];
        wire.readBytes(data);
        wire.release();
        Files.write(output.resolve("sections.bin"),data);
        Files.write(output.resolve("expected-blocks-u32le.bin"),expectedBlocks.array());
        Files.write(output.resolve("expected-biomes-u32le.bin"),expectedBiomes.array());
        Files.writeString(output.resolve("parameters.json"),"{\"protocol\":767,\"min_y\":-64,\"height\":96,\"block_state_count\":65536,\"biome_count\":64,\"section_count\":6,\"generator\":\"Installed Minecraft PalettedContainer.write; synthetic registry identities\"}\n");
        System.out.println("Minecraft encoder produced six sections, " + data.length + " wire bytes");
    }
}
