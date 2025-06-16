export default function PrivacyPage() {
  return (
    <div className="min-h-screen bg-gradient-to-bl from-black via-neutral-900 to-red-900 text-white">
      <div className="container mx-auto px-4 py-8 pt-24">
        <h1 className="text-3xl font-bold text-white mb-6">Privacy</h1>
        <div className="bg-gray-900 p-6 rounded-lg border border-gray-800 max-w-4xl">
          <div className="text-gray-300 space-y-4">
            <p>
              This is a private family ticket management tool. All data is
              stored locally and is only accessible to family members.
            </p>
            <p>
              We do not collect, store, or share any personal information with
              third parties.
            </p>
          </div>
        </div>
      </div>
    </div>
  );
}
