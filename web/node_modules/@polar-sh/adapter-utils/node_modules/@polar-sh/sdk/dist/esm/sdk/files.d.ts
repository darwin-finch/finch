import { ClientSDK, RequestOptions } from "../lib/sdks.js";
import { FileCreate } from "../models/components/filecreate.js";
import { FileUpload } from "../models/components/fileupload.js";
import { FilesDeleteRequest } from "../models/operations/filesdelete.js";
import { FilesListRequest, FilesListResponse } from "../models/operations/fileslist.js";
import { FilesUpdateRequest, FilesUpdateResponseFilesUpdate } from "../models/operations/filesupdate.js";
import { FilesUploadedRequest, FilesUploadedResponseFilesUploaded } from "../models/operations/filesuploaded.js";
import { PageIterator } from "../types/operations.js";
export declare class Files extends ClientSDK {
    /**
     * List Files
     *
     * @remarks
     * List files.
     *
     * **Scopes**: `files:read` `files:write`
     */
    list(request: FilesListRequest, options?: RequestOptions): Promise<PageIterator<FilesListResponse, {
        page: number;
    }>>;
    /**
     * Create File
     *
     * @remarks
     * Create a file.
     *
     * **Scopes**: `files:write`
     */
    create(request: FileCreate, options?: RequestOptions): Promise<FileUpload>;
    /**
     * Complete File Upload
     *
     * @remarks
     * Complete a file upload.
     *
     * **Scopes**: `files:write`
     */
    uploaded(request: FilesUploadedRequest, options?: RequestOptions): Promise<FilesUploadedResponseFilesUploaded>;
    /**
     * Update File
     *
     * @remarks
     * Update a file.
     *
     * **Scopes**: `files:write`
     */
    update(request: FilesUpdateRequest, options?: RequestOptions): Promise<FilesUpdateResponseFilesUpdate>;
    /**
     * Delete File
     *
     * @remarks
     * Delete a file.
     *
     * **Scopes**: `files:write`
     */
    delete(request: FilesDeleteRequest, options?: RequestOptions): Promise<void>;
}
//# sourceMappingURL=files.d.ts.map