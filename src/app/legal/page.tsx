export default function LegalPage() {
  return (
    <div className="min-h-screen bg-gradient-to-tl from-red-900 via-neutral-900 to-black text-white">
      <div className="container mx-auto px-4 py-8">
        <h1 className="text-3xl font-bold text-white mb-6">Legal Notice</h1>
        <div className="bg-gray-900 p-6 rounded-lg border border-gray-800 max-w-4xl">
          <div className="text-gray-300 space-y-4">
            <p>
              <strong>tikkit</strong> - Family Ticket Management System
            </p>
            <p>This software is provided for personal family use only.</p>
            <p>
              All rights reserved. This tool is intended for private family use
              and should not be redistributed or used commercially.
            </p>
          </div>
        </div>
      </div>
    </div>
  );
}
