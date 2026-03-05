import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { HTTPValidationError } from "../models/errors/httpvalidationerror.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { RepositoriesListRequest, RepositoriesListResponse } from "../models/operations/repositorieslist.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
import { PageIterator } from "../types/operations.js";
/**
 * List Repositories
 *
 * @remarks
 * List repositories.
 *
 * **Scopes**: `repositories:read` `repositories:write`
 */
export declare function repositoriesList(client: PolarCore, request: RepositoriesListRequest, options?: RequestOptions): APIPromise<PageIterator<Result<RepositoriesListResponse, HTTPValidationError | SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>, {
    page: number;
}>>;
//# sourceMappingURL=repositoriesList.d.ts.map