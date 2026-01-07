# Simple GenServer for testing Rust interop
# Run with: elixir --sname elixir_test --cookie test_cookie test_server.exs

defmodule TestServer do
  use GenServer

  def start_link(opts \\ []) do
    GenServer.start_link(__MODULE__, :ok, opts)
  end

  @impl true
  def init(:ok) do
    # Trap exits so we receive EXIT messages instead of dying
    Process.flag(:trap_exit, true)
    IO.puts("TestServer started on #{Node.self()}")
    {:ok, %{counter: 0, linked_pids: [], monitored_refs: []}}
  end

  @impl true
  def handle_call(:ping, _from, state) do
    IO.puts("Received :ping, replying :pong")
    {:reply, :pong, state}
  end

  @impl true
  def handle_call({:echo, msg}, _from, state) do
    IO.puts("Received echo request: #{inspect(msg)}")
    {:reply, {:ok, msg}, state}
  end

  @impl true
  def handle_call(:get_counter, _from, state) do
    {:reply, state.counter, state}
  end

  @impl true
  def handle_call(:increment, _from, state) do
    new_state = %{state | counter: state.counter + 1}
    {:reply, new_state.counter, new_state}
  end

  # Link to a remote process
  @impl true
  def handle_call({:link_to, pid}, _from, state) do
    IO.puts("Linking to remote PID: #{inspect(pid)}")
    Process.link(pid)
    new_state = %{state | linked_pids: [pid | state.linked_pids]}
    {:reply, :ok, new_state}
  end

  # Monitor a remote process
  @impl true
  def handle_call({:monitor, pid}, _from, state) do
    IO.puts("Monitoring remote PID: #{inspect(pid)}")
    ref = Process.monitor(pid)
    new_state = %{state | monitored_refs: [{ref, pid} | state.monitored_refs]}
    {:reply, {:ok, ref}, new_state}
  end

  # Spawn a temporary process that can be killed
  @impl true
  def handle_call(:spawn_killable, _from, state) do
    pid = spawn(fn ->
      IO.puts("Killable process started: #{inspect(self())}")
      receive do
        :die -> IO.puts("Killable process dying normally")
        {:die, reason} -> exit(reason)
      end
    end)
    IO.puts("Spawned killable process: #{inspect(pid)}")
    {:reply, {:ok, pid}, state}
  end

  # Kill a process
  @impl true
  def handle_call({:kill, pid, reason}, _from, state) do
    IO.puts("Killing process #{inspect(pid)} with reason: #{inspect(reason)}")
    Process.exit(pid, reason)
    {:reply, :ok, state}
  end

  # Spawn a process on a remote node (for testing SpawnRequest)
  @impl true
  def handle_call({:spawn_on_node, node, module, function, args}, _from, state) do
    IO.puts("Spawning #{module}.#{function}/#{length(args)} on node #{node}")
    try do
      pid = Node.spawn(node, module, function, args)
      IO.puts("Spawned remote process: #{inspect(pid)}")
      {:reply, {:ok, pid}, state}
    rescue
      e ->
        IO.puts("Failed to spawn: #{inspect(e)}")
        {:reply, {:error, inspect(e)}, state}
    catch
      kind, reason ->
        IO.puts("Failed to spawn (#{kind}): #{inspect(reason)}")
        {:reply, {:error, {kind, reason}}, state}
    end
  end

  # Spawn and link to a process on a remote node
  @impl true
  def handle_call({:spawn_link_on_node, node, module, function, args}, _from, state) do
    IO.puts("Spawning linked #{module}.#{function}/#{length(args)} on node #{node}")
    try do
      pid = Node.spawn_link(node, module, function, args)
      IO.puts("Spawned linked remote process: #{inspect(pid)}")
      new_state = %{state | linked_pids: [pid | state.linked_pids]}
      {:reply, {:ok, pid}, new_state}
    rescue
      e ->
        IO.puts("Failed to spawn_link: #{inspect(e)}")
        {:reply, {:error, inspect(e)}, state}
    catch
      kind, reason ->
        IO.puts("Failed to spawn_link (#{kind}): #{inspect(reason)}")
        {:reply, {:error, {kind, reason}}, state}
    end
  end

  @impl true
  def handle_cast({:print, msg}, state) do
    IO.puts("Cast received: #{inspect(msg)}")
    {:noreply, state}
  end

  @impl true
  def handle_info({:DOWN, ref, :process, pid, reason}, state) do
    IO.puts("DOWN received: ref=#{inspect(ref)}, pid=#{inspect(pid)}, reason=#{inspect(reason)}")
    new_refs = Enum.reject(state.monitored_refs, fn {r, _} -> r == ref end)
    {:noreply, %{state | monitored_refs: new_refs}}
  end

  @impl true
  def handle_info({:EXIT, pid, reason}, state) do
    IO.puts("EXIT received: pid=#{inspect(pid)}, reason=#{inspect(reason)}")
    new_links = Enum.reject(state.linked_pids, &(&1 == pid))
    {:noreply, %{state | linked_pids: new_links}}
  end

  @impl true
  def handle_info(msg, state) do
    IO.puts("Info received: #{inspect(msg)}")
    {:noreply, state}
  end
end

# Start the server and register it
{:ok, pid} = TestServer.start_link(name: :test_server)
IO.puts("TestServer registered as :test_server with PID #{inspect(pid)}")
IO.puts("Node: #{Node.self()}")
IO.puts("Cookie: #{Node.get_cookie()}")
IO.puts("")
IO.puts("Waiting for connections... (Ctrl+C to exit)")
IO.puts("From Rust, connect to: #{Node.self()} with cookie 'test_cookie'")
IO.puts("")

# Keep the script running
Process.sleep(:infinity)
