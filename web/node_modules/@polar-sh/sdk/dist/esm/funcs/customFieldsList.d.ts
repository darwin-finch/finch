import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { HTTPValidationError } from "../models/errors/httpvalidationerror.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { CustomFieldsListRequest, CustomFieldsListResponse } from "../models/operations/customfieldslist.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
import { PageIterator } from "../types/operations.js";
/**
 * List Custom Fields
 *
 * @remarks
 * List custom fields.
 *
 * **Scopes**: `custom_fields:read` `custom_fields:write`
 */
export declare function customFieldsList(client: PolarCore, request: CustomFieldsListRequest, options?: RequestOptions): APIPromise<PageIterator<Result<CustomFieldsListResponse, HTTPValidationError | SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>, {
    page: number;
}>>;
//# sourceMappingURL=customFieldsList.d.ts.map