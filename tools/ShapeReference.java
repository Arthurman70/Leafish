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
                case "float": return float.class;
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
    static double[] vector(Object value, Field[] fields) throws Exception {
        double[] result = new double[3];
        for (int i=0;i<3;i++) result[i]=fields[i].getDouble(value);
        return result;
    }
    static double[] offsetAt(JsonObject descriptor, int[] position) {
        // BlockBehaviour.Properties.offsetType and Mth.getSeed in original 1.21.1.
        // The X multiplication is intentionally an overflowing Java int first.
        long seed=(long)(position[0]*3129871) ^ (long)position[2]*116129781L;
        seed=seed*seed*42317861L+seed*11L; seed >>= 16;
        double limit=descriptor.get("max_horizontal").getAsDouble();
        double x=Math.max(-limit,Math.min(limit,((double)((float)(seed&15L)/15.0F)-0.5)*0.5));
        double z=Math.max(-limit,Math.min(limit,((double)((float)(seed>>8&15L)/15.0F)-0.5)*0.5));
        double y=descriptor.get("kind").getAsString().equals("xyz")
            ? ((double)((float)(seed>>4&15L)/15.0F)-1.0)*descriptor.get("max_vertical").getAsDouble() : 0.0;
        return new double[]{x,y,z};
    }
    static JsonArray translated(JsonArray boxes, double[] offset, double sign) {
        JsonArray result=new JsonArray();
        for(JsonElement box:boxes) {
            JsonArray moved=new JsonArray();
            for(int i=0;i<6;i++) moved.add(box.getAsJsonArray().get(i).getAsDouble()+sign*offset[i%3]);
            result.add(moved);
        }
        return result;
    }
    static boolean sameBoxes(JsonArray a, JsonArray b) {
        if(a.size()!=b.size()) return false;
        for(int i=0;i<a.size();i++) for(int j=0;j<6;j++)
            if(Math.abs(a.get(i).getAsJsonArray().get(j).getAsDouble()-b.get(i).getAsJsonArray().get(j).getAsDouble())>1e-12) return false;
        return true;
    }
    static final int[][] OFFSET_POSITIONS=offsetPositions();
    static int[][] offsetPositions() {
        int[][] values=new int[68][3];
        values[0]=new int[]{0,0,0}; values[1]=new int[]{-1,-64,-1};
        values[2]=new int[]{29999999,319,-29999999}; values[3]=new int[]{-29999999,255,29999999};
        for(int i=4;i<values.length;i++) values[i]=new int[]{i*104729-3000000,i%384-64,i*130363-4000000};
        return values;
    }
    static JsonArray verifiedOffsetShape(Object state, Method shape, Method getOffset, JsonObject descriptor,
            Object world, Object context, Constructor<?> position, Method toAabbs, Field[] coords,
            Field[] vectorFields, JsonObject result, String kind) throws Exception {
        Object zero=position.newInstance(0,0,0);
        JsonArray original=boxes(shape.invoke(state,world,zero,context),toAabbs,coords);
        JsonArray base=translated(original,vector(getOffset.invoke(state,world,zero),vectorFields),-1.0);
        boolean constant=true, shifted=true;
        for(int[] at:OFFSET_POSITIONS) {
            Object pos=position.newInstance(at[0],at[1],at[2]);
            JsonArray actual=boxes(shape.invoke(state,world,pos,context),toAabbs,coords);
            constant &= sameBoxes(actual,original);
            shifted &= sameBoxes(actual,translated(base,offsetAt(descriptor,at),1.0));
        }
        if(!constant && !shifted) throw new IllegalStateException("Offset shape is not a verified translation");
        result.addProperty(kind+"_offset",!constant);
        return constant?original:base;
    }
    public static void main(String[] args) throws Exception {
        if (args.length != 4) throw new IllegalArgumentException("Expected mappings blocks.json official-server.jar output.json");
        Path mappingPath=Path.of(args[0]), catalogPath=Path.of(args[1]), serverPath=Path.of(args[2]), output=Path.of(args[3]);
        if (Files.exists(output)) throw new IllegalArgumentException("Refusing to overwrite existing shape report");
        Path offsetOutput=output.resolveSibling("offset-reference.json");
        if (Files.exists(offsetOutput)) throw new IllegalArgumentException("Refusing to overwrite existing offset reference");
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
        Method getOffset=map.method(stateBase,"getOffset",getter,position);
        Method maxHorizontal=map.method("net.minecraft.world.level.block.state.BlockBehaviour","getMaxHorizontalOffset");
        Method maxVertical=map.method("net.minecraft.world.level.block.state.BlockBehaviour","getMaxVerticalOffset");
        Method friction=map.method(block,"getFriction"), speedFactor=map.method(block,"getSpeedFactor"), jumpFactor=map.method(block,"getJumpFactor");
        Constructor<?> blockPosition=map.type(position).getConstructor(int.class,int.class,int.class);
        Field[] vectorFields={map.field("net.minecraft.world.phys.Vec3","x"),map.field("net.minecraft.world.phys.Vec3","y"),map.field("net.minecraft.world.phys.Vec3","z")};
        Method collision=map.method(stateBase,"getCollisionShape",getter,position);
        Method collisionContext=map.method(stateBase,"getCollisionShape",getter,position,context);
        Method outline=map.method(stateBase,"getShape",getter,position,context);
        Method aabbs=map.method("net.minecraft.world.phys.shapes.VoxelShape","toAabbs");
        Constructor<?> playerContext=map.type("net.minecraft.world.phys.shapes.EntityCollisionContext").getDeclaredConstructor(
            boolean.class,double.class,map.type("net.minecraft.world.item.ItemStack"),java.util.function.Predicate.class,map.type("net.minecraft.world.entity.Entity"));
        playerContext.setAccessible(true);
        Object emptyItem=map.field("net.minecraft.world.item.ItemStack","EMPTY").get(null);
        Method fluidAt=map.method(getter,"getFluidState",position);
        Object fluidWorld=Proxy.newProxyInstance(ShapeReference.class.getClassLoader(),new Class<?>[]{map.type(getter)},
            (proxy,method,values)->{
                if(method.getName().equals(fluidAt.getName()) && method.getParameterCount()==1) return fluidAt.invoke(empty,values);
                throw new ContextRead("world lookup "+method.getName());
            });
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
        JsonArray offsetReference=new JsonArray();
        Set<String> referenceNames=new HashSet<>(List.of("minecraft:dandelion","minecraft:short_grass","minecraft:bamboo","minecraft:pointed_dripstone"));
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
                JsonObject movement=new JsonObject();
                movement.addProperty("friction",(Float)friction.invoke(owner));
                movement.addProperty("speed_factor",(Float)speedFactor.invoke(owner));
                movement.addProperty("jump_factor",(Float)jumpFactor.invoke(owner));
                item.add("movement",movement);
                item.add("offset",JsonNull.INSTANCE); item.addProperty("collision_offset",false); item.addProperty("outline_offset",false);
                item.addProperty("collision_context","independent");
                JsonObject descriptor=null;
                if(hasOffset) {
                    descriptor=new JsonObject();
                    double[] originOffset=vector(getOffset.invoke(blockState,strictWorld,zero),vectorFields);
                    descriptor.addProperty("kind",originOffset[1]==0.0?"xz":"xyz");
                    descriptor.addProperty("max_horizontal",(double)(Float)maxHorizontal.invoke(owner));
                    descriptor.addProperty("max_vertical",(double)(Float)maxVertical.invoke(owner));
                    for(int[] at:OFFSET_POSITIONS) {
                        double[] actual=vector(getOffset.invoke(blockState,strictWorld,blockPosition.newInstance(at[0],at[1],at[2])),vectorFields);
                        if(!Arrays.equals(actual,offsetAt(descriptor,at))) throw new IllegalStateException("Offset formula differs from original runtime at state "+id);
                    }
                    item.add("offset",descriptor);
                }
                item.add("collision",JsonNull.INSTANCE); item.add("outline",JsonNull.INSTANCE);
                String collisionProblem=isDynamic&&!hasOffset?"dynamic shape":null;
                if(collisionProblem==null) {
                    try {
                        JsonArray resolved;
                        if(hasOffset) {
                            resolved=verifiedOffsetShape(blockState,collisionContext,getOffset,descriptor,strictWorld,strictContext,blockPosition,aabbs,coords,vectorFields,item,"collision");
                        } else if(name.equals("minecraft:water") || name.equals("minecraft:lava")) {
                            // Vanilla Player inherits LivingEntity.canStandOnFluid=false.
                            // Invoke the real EntityCollisionContext above and below the fluid.
                            resolved=null;
                            for(double feet:new double[]{-2.0,0.25,2.0}) {
                                Object ctx=playerContext.newInstance(false,feet,emptyItem,(java.util.function.Predicate<Object>)(fluid->false),null);
                                JsonArray actual=boxes(collisionContext.invoke(blockState,fluidWorld,zero,ctx),aabbs,coords);
                                if(resolved!=null && !sameBoxes(resolved,actual)) throw new IllegalStateException("Fluid player collision changes with position");
                                resolved=actual;
                            }
                            if(!resolved.isEmpty()) throw new IllegalStateException("Unexpected vanilla player fluid support");
                            item.addProperty("collision_context","vanilla_player_no_fluid_standing");
                        } else {
                            resolved=boxes(collision.invoke(blockState,empty,zero),aabbs,coords);
                            JsonArray strict=boxes(collisionContext.invoke(blockState,strictWorld,zero,strictContext),aabbs,coords);
                            if(!resolved.equals(strict)) throw new IllegalStateException("Collision cache differs from context-free invocation");
                        }
                        item.add("collision",resolved); collisionKnown++;
                    } catch(Exception error) { collisionProblem=failure(error); }
                }
                String outlineProblem=isDynamic&&!hasOffset?"dynamic shape":null;
                if(outlineProblem==null) {
                    try {
                        item.add("outline",hasOffset
                            ? verifiedOffsetShape(blockState,outline,getOffset,descriptor,strictWorld,strictContext,blockPosition,aabbs,coords,vectorFields,item,"outline")
                            : boxes(outline.invoke(blockState,strictWorld,zero,strictContext),aabbs,coords));
                        outlineKnown++;
                    }
                    catch(Exception error) { outlineProblem=failure(error); }
                }
                if(collisionProblem==null) item.add("collision_unresolved",JsonNull.INSTANCE);
                else { item.addProperty("collision_unresolved",collisionProblem); unresolvedCollision.merge(name,1,Integer::sum); }
                if(outlineProblem==null) item.add("outline_unresolved",JsonNull.INSTANCE);
                else { item.addProperty("outline_unresolved",outlineProblem); unresolvedOutline.merge(name,1,Integer::sum); }
                if(referenceNames.remove(name)) {
                    if(collisionProblem!=null || outlineProblem!=null) throw new IllegalStateException("Reference sample remains unresolved");
                    for(int[] at:OFFSET_POSITIONS) {
                        Object pos=blockPosition.newInstance(at[0],at[1],at[2]);
                        JsonObject sample=new JsonObject(); sample.addProperty("id",id); sample.addProperty("name",name);
                        JsonArray point=new JsonArray(), actualOffset=new JsonArray();
                        for(int value:at) point.add(value);
                        for(double value:vector(getOffset.invoke(blockState,strictWorld,pos),vectorFields)) actualOffset.add(value);
                        sample.add("position",point); sample.add("offset",actualOffset);
                        // These are original runtime method outputs, not the reconstructed formula/boxes.
                        sample.add("collision",boxes(collisionContext.invoke(blockState,strictWorld,pos,strictContext),aabbs,coords));
                        sample.add("outline",boxes(outline.invoke(blockState,strictWorld,pos,strictContext),aabbs,coords));
                        offsetReference.add(sample);
                    }
                }
                if(results.put(id,item)!=null) throw new IllegalStateException("Duplicate runtime ID");
            }
        }
        if(!results.keySet().equals(expected.keySet())) throw new IllegalStateException("Runtime states differ from catalog domain");
        for(int id=0;id<results.size();id++) if(!results.containsKey(id)) throw new IllegalStateException("Nondense state domain");
        JsonObject report=new JsonObject();
        report.addProperty("schema_version",2); report.addProperty("minecraft_version","1.21.1");
        report.addProperty("block_catalog_sha256",sha256(catalogPath));
        report.addProperty("official_server_sha256",sha256(serverPath));
        report.addProperty("official_server_mappings_sha256",sha256(mappingPath));
        report.addProperty("generator","official Minecraft 1.21.1 server runtime reflection; no world loaded");
        report.addProperty("offset_validation_positions_per_state",OFFSET_POSITIONS.length);
        report.addProperty("fluid_collision_context","vanilla living player, cannot stand on fluids; no fluid movement physics implied");
        report.addProperty("collision_resolved",collisionKnown); report.addProperty("outline_resolved",outlineKnown);
        JsonArray states=new JsonArray(); results.values().forEach(states::add); report.add("states",states);
        Gson gson=new GsonBuilder().serializeNulls().create();
        if(!referenceNames.isEmpty() || offsetReference.size()!=272) throw new IllegalStateException("Incomplete offset reference samples");
        Files.writeString(output,gson.toJson(report)+"\n",StandardCharsets.UTF_8,StandardOpenOption.CREATE_NEW);
        Files.writeString(offsetOutput,gson.toJson(offsetReference)+"\n",StandardCharsets.UTF_8,StandardOpenOption.CREATE_NEW);
        System.out.println("Verified states="+results.size()+" collision="+collisionKnown+" outline="+outlineKnown);
        System.out.println("Unresolved collision="+gson.toJson(unresolvedCollision));
        System.out.println("Unresolved outline="+gson.toJson(unresolvedOutline));
    }
}
