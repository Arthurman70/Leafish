package leafishreference;

import com.google.gson.*;
import java.lang.reflect.*;
import java.nio.charset.StandardCharsets;
import java.nio.file.*;
import java.security.MessageDigest;
import java.util.*;

/** Read-only reference extraction from the original vanilla 1.21.1 server.
 * Reflection names come from that exact version's official server mappings.
 * No server, world, entity, or mod loader is started. Generated data stays local.
 * Run through generate_shapes.py to validate and isolate all runtime inputs.
 */
public final class ShapeReference {
    static final class Mapping {
        final Map<String,String> classes = new HashMap<>();
        final Map<String,String> members = new HashMap<>();
        Mapping(Path path) throws Exception {
            String owner = null;
            for (String line : Files.readAllLines(path)) {
                if (line.startsWith("#") || !line.contains(" -> ")) continue;
                String[] parts = line.trim().split(" -> ", 2);
                if (!line.startsWith(" ")) {
                    owner = parts[0]; classes.put(owner, parts[1].replace(":", ""));
                } else {
                    String member = parts[0].replaceFirst("^\\d+:\\d+:", "");
                    int space = member.indexOf(' ');
                    member = member.substring(space + 1);
                    int end = member.indexOf(')');
                    if (end >= 0) member = member.substring(0, end + 1);
                    members.put(owner + "#" + member, parts[1]);
                }
            }
        }
        Class<?> type(String name) throws Exception {
            switch (name) {
                case "boolean": return boolean.class;
                case "int": return int.class;
                case "double": return double.class;
                default: return Class.forName(classes.getOrDefault(name, name));
            }
        }
        Method method(String owner, String name, String... parameters) throws Exception {
            String key = owner + "#" + name + "(" + String.join(",", parameters) + ")";
            String mapped = Objects.requireNonNull(members.get(key), key);
            Class<?>[] types = new Class<?>[parameters.length];
            for (int i=0;i<types.length;i++) types[i] = type(parameters[i]);
            Method result = type(owner).getDeclaredMethod(mapped, types);
            result.setAccessible(true); return result;
        }
        Field field(String owner, String name) throws Exception {
            String key = owner + "#" + name;
            Field result = type(owner).getDeclaredField(Objects.requireNonNull(members.get(key), key));
            result.setAccessible(true); return result;
        }
    }
    static final class ContextRead extends RuntimeException {
        ContextRead(String name) { super(name); }
    }
    static JsonArray boxes(Object shape, Method toAabbs, Field[] coordinates) throws Exception {
        JsonArray result = new JsonArray();
        for (Object box : (List<?>)toAabbs.invoke(shape)) {
            JsonArray values = new JsonArray();
            for (Field field : coordinates) {
                double value = field.getDouble(box);
                if (!Double.isFinite(value)) throw new IllegalStateException("Nonfinite shape");
                values.add(value);
            }
            result.add(values);
        }
        return result;
    }
    static String sha256(Path path) throws Exception {
        return HexFormat.of().formatHex(MessageDigest.getInstance("SHA-256").digest(Files.readAllBytes(path)));
    }
    static String failure(Throwable error) {
        while (error.getCause() != null) error = error.getCause();
        if (error instanceof ContextRead) return "requires " + error.getMessage();
        return "reference method failed: " + error.getClass().getSimpleName();
    }
    public static void main(String[] args) throws Exception {
        if (args.length != 4) throw new IllegalArgumentException("Expected mappings blocks.json official-server.jar output.json");
        Path mappingPath=Path.of(args[0]), catalogPath=Path.of(args[1]), serverPath=Path.of(args[2]), output=Path.of(args[3]);
        if (Files.exists(output)) throw new IllegalArgumentException("Refusing to overwrite existing shape report");
        Mapping map = new Mapping(mappingPath);
        map.method("net.minecraft.SharedConstants", "tryDetectVersion").invoke(null);
        map.method("net.minecraft.server.Bootstrap", "bootStrap").invoke(null);
        String block="net.minecraft.world.level.block.Block", state="net.minecraft.world.level.block.state.BlockState";
        String stateBase="net.minecraft.world.level.block.state.BlockBehaviour$BlockStateBase";
        String getter="net.minecraft.world.level.BlockGetter", position="net.minecraft.core.BlockPos";
        String context="net.minecraft.world.phys.shapes.CollisionContext";
        Object registry=map.field("net.minecraft.core.registries.BuiltInRegistries", "BLOCK").get(null);
        Object empty=map.field("net.minecraft.world.level.EmptyBlockGetter", "INSTANCE").get(null);
        Object zero=map.field(position,"ZERO").get(null);
        Object strictWorld=Proxy.newProxyInstance(ShapeReference.class.getClassLoader(), new Class<?>[]{map.type(getter)},
            (proxy, method, values) -> { throw new ContextRead("world lookup " + method.getName()); });
        Object strictContext=Proxy.newProxyInstance(ShapeReference.class.getClassLoader(), new Class<?>[]{map.type(context)},
            (proxy, method, values) -> { throw new ContextRead("player collision context " + method.getName()); });
        Method nameOf=map.method("net.minecraft.core.Registry","getKey","java.lang.Object");
        Method definition=map.method(block,"getStateDefinition");
        Method possible=map.method("net.minecraft.world.level.block.state.StateDefinition","getPossibleStates");
        Method idOf=map.method(block,"getId",state), dynamic=map.method(block,"hasDynamicShape");
        Method properties=map.method("net.minecraft.world.level.block.state.StateHolder","getValues");
        Method propertyName=map.method("net.minecraft.world.level.block.state.properties.Property","getName");
        Method valueName=map.method("net.minecraft.world.level.block.state.properties.Property","getName","java.lang.Comparable");
        Method offset=map.method(stateBase,"hasOffsetFunction");
        Method collision=map.method(stateBase,"getCollisionShape",getter,position);
        Method collisionContext=map.method(stateBase,"getCollisionShape",getter,position,context);
        Method outline=map.method(stateBase,"getShape",getter,position,context);
        Method aabbs=map.method("net.minecraft.world.phys.shapes.VoxelShape","toAabbs");
        Field[] coords=new Field[6];
        String[] axes={"minX","minY","minZ","maxX","maxY","maxZ"};
        for(int i=0;i<6;i++) coords[i]=map.field("net.minecraft.world.phys.AABB",axes[i]);
        JsonObject catalog=JsonParser.parseString(Files.readString(catalogPath)).getAsJsonObject();
        TreeMap<Integer,JsonObject> expected=new TreeMap<>(), results=new TreeMap<>();
        for (var entry:catalog.entrySet()) for (JsonElement value:entry.getValue().getAsJsonObject().getAsJsonArray("states")) {
            JsonObject expectedState=value.getAsJsonObject().deepCopy(); expectedState.addProperty("name",entry.getKey());
            if (!expectedState.has("properties")) expectedState.add("properties",new JsonObject());
            if (expected.put(expectedState.get("id").getAsInt(),expectedState)!=null) throw new IllegalStateException("Duplicate catalog ID");
        }
        int collisionKnown=0, outlineKnown=0;
        TreeMap<String,Integer> unresolvedCollision=new TreeMap<>(), unresolvedOutline=new TreeMap<>();
        for (Object owner:(Iterable<?>)registry) {
            String name=nameOf.invoke(registry,owner).toString();
            for (Object blockState:(List<?>)possible.invoke(definition.invoke(owner))) {
                int id=(Integer)idOf.invoke(null,blockState);
                JsonObject item=new JsonObject(), props=new JsonObject();
                TreeMap<String,String> sortedProperties=new TreeMap<>();
                for(var entry:((Map<?,?>)properties.invoke(blockState)).entrySet()) {
                    sortedProperties.put((String)propertyName.invoke(entry.getKey()),(String)valueName.invoke(entry.getKey(),entry.getValue()));
                }
                sortedProperties.forEach(props::addProperty);
                JsonObject target=expected.get(id);
                if(target==null || !name.equals(target.get("name").getAsString()) || !props.equals(target.getAsJsonObject("properties"))) {
                    throw new IllegalStateException("State identity differs from vanilla catalog at " + id + ": " + name + props);
                }
                boolean isDynamic=(Boolean)dynamic.invoke(owner), hasOffset=(Boolean)offset.invoke(blockState);
                item.addProperty("id",id); item.addProperty("name",name); item.add("properties",props);
                item.addProperty("dynamic",isDynamic); item.addProperty("has_offset",hasOffset);
                item.add("collision",JsonNull.INSTANCE); item.add("outline",JsonNull.INSTANCE);
                String collisionProblem=isDynamic?"dynamic shape":null;
                if(collisionProblem==null) {
                    try {
                        JsonArray cached=boxes(collision.invoke(blockState,empty,zero),aabbs,coords);
                        JsonArray strict=boxes(collisionContext.invoke(blockState,strictWorld,zero,strictContext),aabbs,coords);
                        if(!cached.equals(strict)) throw new IllegalStateException("Collision cache differs from context-free invocation");
                        item.add("collision",cached); collisionKnown++;
                    } catch(Exception error) { collisionProblem=failure(error); }
                }
                String outlineProblem=isDynamic?"dynamic shape":hasOffset?"position-dependent offset":null;
                if(outlineProblem==null) {
                    try { item.add("outline",boxes(outline.invoke(blockState,strictWorld,zero,strictContext),aabbs,coords)); outlineKnown++; }
                    catch(Exception error) { outlineProblem=failure(error); }
                }
                if(collisionProblem==null) item.add("collision_unresolved",JsonNull.INSTANCE);
                else { item.addProperty("collision_unresolved",collisionProblem); unresolvedCollision.merge(name,1,Integer::sum); }
                if(outlineProblem==null) item.add("outline_unresolved",JsonNull.INSTANCE);
                else { item.addProperty("outline_unresolved",outlineProblem); unresolvedOutline.merge(name,1,Integer::sum); }
                if(results.put(id,item)!=null) throw new IllegalStateException("Duplicate runtime ID");
            }
        }
        if(!results.keySet().equals(expected.keySet())) throw new IllegalStateException("Runtime states differ from catalog domain");
        for(int id=0;id<results.size();id++) if(!results.containsKey(id)) throw new IllegalStateException("Nondense state domain");
        JsonObject report=new JsonObject();
        report.addProperty("schema_version",1); report.addProperty("minecraft_version","1.21.1");
        report.addProperty("block_catalog_sha256",sha256(catalogPath));
        report.addProperty("official_server_sha256",sha256(serverPath));
        report.addProperty("official_server_mappings_sha256",sha256(mappingPath));
        report.addProperty("generator","official Minecraft 1.21.1 server runtime reflection; no world loaded");
        report.addProperty("collision_resolved",collisionKnown); report.addProperty("outline_resolved",outlineKnown);
        JsonArray states=new JsonArray(); results.values().forEach(states::add); report.add("states",states);
        Gson gson=new GsonBuilder().serializeNulls().create();
        Files.writeString(output,gson.toJson(report)+"\n",StandardCharsets.UTF_8,StandardOpenOption.CREATE_NEW);
        System.out.println("Verified states="+results.size()+" collision="+collisionKnown+" outline="+outlineKnown);
        System.out.println("Unresolved collision="+gson.toJson(unresolvedCollision));
        System.out.println("Unresolved outline="+gson.toJson(unresolvedOutline));
    }
}
